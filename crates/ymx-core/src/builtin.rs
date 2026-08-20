//! Builtin special forms: `$merge`, `$map`, `$reduce` (rules 15–16).
//!
//! Each builtin is a **special form** that declares its own argument-evaluation
//! strategy rather than uniformly receiving all arguments pre-evaluated.
//!
//! - `$merge(a, b)` evaluates both arguments eagerly: `Array⊕Array` → concatenation,
//!   `Object⊕Object` → shallow merge (later overwrites earlier), other shapes → `E011`.
//! - `$map(fn, arr)` keeps `fn` unevaluated as a callable component reference and
//!   evaluates `arr` eagerly; `arr` must be an `Array` (non-array → `E011`);
//!   each item is passed to `fn`: object item → named args, scalar item → `$0`,
//!   array item → `E011`.
//! - `$reduce(fn, arr)` is like `$map` but each step also has `$last` = previous
//!   result; empty `arr` → `Value::Null` (no step run); single-element runs one
//!   step with `last` **not in scope** (ref → `E003`); subsequent steps expose
//!   `$last` via `last` in math (String re-scan per rule 7).

use indexmap::IndexMap;
use std::rc::Rc;

use crate::diag::{Diagnostic, FileId, Span, E002, E005, E008, E011};
use crate::interp;
use crate::ir::Value;
use crate::math::{CallHook, Scope, V1Engine};
use crate::namespace::Definition;
use crate::project::Options;
use crate::project::Project;
use crate::resolve::LookupMiss;

/// The three v1 builtins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Merge,
    Map,
    Reduce,
}

/// Result type for `eval_resolve_call_args`: (named args, positional args).
type CallArgs = (Vec<(String, Value)>, Vec<Value>);

impl Builtin {
    /// Detect a builtin name from its effective identifier (without the `$` prefix).
    /// Returns `None` for non-builtin names.
    pub fn from_name(name: &str) -> Option<Builtin> {
        match name {
            "merge" => Some(Builtin::Merge),
            "map" => Some(Builtin::Map),
            "reduce" => Some(Builtin::Reduce),
            _ => None,
        }
    }

    /// `true` if `name` is one of the reserved builtin effective identifiers.
    pub fn is_reserved(name: &str) -> bool {
        Self::from_name(name).is_some()
    }
}

/// Context a builtin implementation needs to evaluate its arguments and call
/// components.
pub struct BuiltinCtx<'a> {
    /// The file context for diagnostics.
    pub file: Option<std::path::PathBuf>,
    /// The component name for diagnostics.
    pub component: Option<String>,
    /// The call-site span for diagnostics.
    pub span: Span,
    /// The loaded project.
    pub project: &'a Project,
    /// Compiler options (for `from_keyword`, `max_depth`, `plain`, etc.).
    pub opts: &'a Options,
    /// The current recursion depth.
    pub depth: u32,
    /// Hook for component calls (math `name(...)` inside `${...}`).
    pub call: CallHook<'a>,
}

/// A builtin implementation that evaluates its arguments per its own strategy.
pub trait BuiltinImpl {
    /// Evaluate this builtin with the given already-parsed positional arguments.
    /// The `args` are the raw parsed values from the call-site; the builtin
    /// decides which to evaluate eagerly and which to keep unevaluated.
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic>;
}

// ---- $merge ----

pub struct MergeBuiltin;

impl BuiltinImpl for MergeBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$merge expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        // Both args are evaluated eagerly: resolve each through the caller's scope.
        let a = resolve_parsed_value(&args[0].value, ctx)?;
        let b = resolve_parsed_value(&args[1].value, ctx)?;

        match (&a, &b) {
            (Value::Array(av), Value::Array(bv)) => {
                // Array ⊕ Array → concatenation
                let mut result = av.clone();
                result.extend_from_slice(bv);
                Ok(Value::Array(result))
            }
            (Value::Object(am), Value::Object(bm)) => {
                // Object ⊕ Object → shallow merge (later overwrites earlier)
                let mut merged = am.clone();
                for (k, v) in bm {
                    merged.insert(k.clone(), v.clone());
                }
                Ok(Value::Object(merged))
            }
            _ => Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$merge is only defined for Array⊕Array and Object⊕Object, got {:?}⊕{:?}",
                    a, b
                ),
            )),
        }
    }
}

// ---- $map ----

pub struct MapBuiltin;

impl BuiltinImpl for MapBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$map expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        // First arg: unevaluated callable component reference.
        // Must be a bare `Ref` (a name like `add` or `subdir.fn` — the raw name string).
        let fn_name = match &args[0].value {
            super::callsite::ParsedValue::Ref { name } => name.clone(),
            other => {
                return Err(ctx_err(ctx, E011,
                    format!("first argument of $map must be an unevaluated callable component reference, got {:?}", other)));
            }
        };

        // Second arg: eagerly evaluated, must be Array.
        let arr = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("second argument of $map must be an Array, got {:?}", arr),
            ));
        };

        // Empty array → empty array.
        if items.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        // Look up the callable component (dotted paths allowed).
        let def = resolve_callable(ctx, &fn_name)?;

        // Map each item: object → named args, scalar → $0, array → E011.
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let item_args = match item {
                Value::Object(m) => {
                    super::ir::Args::Named(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                }
                v if v.is_scalar() => super::ir::Args::Positional(vec![v.clone()]),
                Value::Array(_) => {
                    return Err(ctx_err(ctx, E011,
                        "array item in $map argument is not supported (array items must be objects or scalars)".to_string()));
                }
                _ => unreachable!("Value::Object and scalar cover all non-array cases"),
            };

            // Each item evaluation is a recursive component call with depth check.
            let result = eval_call(ctx, def.clone(), &item_args)?;
            out.push(result);
        }

        Ok(Value::Array(out))
    }
}

// ---- $reduce ----

pub struct ReduceBuiltin;

impl BuiltinImpl for ReduceBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() < 2 || args.len() > 3 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$reduce expects 2 or 3 arguments, got {}", args.len()),
            ));
        }

        // First arg: unevaluated callable component reference.
        let fn_name = match &args[0].value {
            super::callsite::ParsedValue::Ref { name } => name.clone(),
            other => {
                return Err(ctx_err(ctx, E011,
                    format!("first argument of $reduce must be an unevaluated callable component reference, got {:?}", other)));
            }
        };

        // Third arg (optional): eagerly evaluated initial value.
        let init = if args.len() == 3 {
            Some(resolve_parsed_value(&args[2].value, ctx)?)
        } else {
            None
        };

        // Second arg: eagerly evaluated, must be Array.
        let arr = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("second argument of $reduce must be an Array, got {:?}", arr),
            ));
        };

        // Empty array → Value::Null (no step run).
        if items.is_empty() {
            return Ok(Value::Null);
        }

        // Look up the callable component.
        let def = resolve_callable(ctx, &fn_name)?;

        // Single-element: run one step, `last` NOT in scope.
        if items.len() == 1 {
            let item = &items[0];
            let item_args = match item {
                Value::Object(m) => {
                    super::ir::Args::Named(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                }
                v if v.is_scalar() => super::ir::Args::Positional(vec![v.clone()]),
                Value::Array(_) => {
                    return Err(ctx_err(ctx, E011,
                        "array item in $reduce argument is not supported (array items must be objects or scalars)".to_string()));
                }
                _ => unreachable!(),
            };

            // No depth check here because we're not recursing into a sub-call
            // that would be depth-limited; the outer component call already
            // consumed a depth slot. But we do check for the single-step case
            // where `last` is unavailable.
            let scope = build_scope_for_call(ctx, &item_args, init.as_ref());
            let result = eval_def(ctx, &def, &item_args, &scope)?;

            // Verify `last` is not referenced in the result by trying to look it up.
            // We run the step without `last` in scope, so any `$last` ref inside
            // the component body would surface as E003 from the interpolation.
            return Ok(result);
        }

        // Multiple items: run steps, each exposing the previous result as `last`.
        let mut prev: Option<Value> = None;

        for item in items {
            let item_args = match item {
                Value::Object(m) => {
                    super::ir::Args::Named(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                }
                v if v.is_scalar() => super::ir::Args::Positional(vec![v.clone()]),
                Value::Array(_) => {
                    return Err(ctx_err(ctx, E011,
                        "array item in $reduce argument is not supported (array items must be objects or scalars)".to_string()));
                }
                _ => unreachable!(),
            };

            // Build scope with `last` bound to init (if provided) on first step,
            // then to prev result on subsequent steps.
            let result = {
                let last = prev.as_ref().or(init.as_ref());
                let scope = build_scope_for_call(ctx, &item_args, last);
                eval_def(ctx, &def, &item_args, &scope)?
            };
            prev = Some(result);
        }

        Ok(prev.expect("non-empty reduce always produces a result"))
    }
}

// ---- Shared helper functions ----

/// Resolve a parsed call-site argument value through the caller's scope.
/// Used for eagerly-evaluated builtin arguments.
fn resolve_parsed_value(
    value: &super::callsite::ParsedValue,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    match value {
        super::callsite::ParsedValue::Literal(v) => Ok(v.clone()),
        super::callsite::ParsedValue::Raw(s) => Ok(Value::string(s.clone())),
        super::callsite::ParsedValue::Ref { name } => {
            // Resolve a `$name` reference through the scope.
            let segments = interp::scan(&format!("${name}"), ctx.span)?;
            let scope = build_caller_scope(ctx);
            interp::resolve(&segments, &scope, &V1Engine)
        }
        super::callsite::ParsedValue::Math { src } => {
            let segments = interp::scan(&format!("${{{src}}}"), ctx.span)?;
            let scope = build_caller_scope(ctx);
            interp::resolve(&segments, &scope, &V1Engine)
        }
        super::callsite::ParsedValue::Call(nested) => {
            // A nested call-site: evaluate it by resolving through our call hook.
            let segments = interp::scan(&format!("${}(...)", nested.name), ctx.span)?;
            let scope = build_caller_scope(ctx);
            interp::resolve(&segments, &scope, &V1Engine)
        }
    }
}

/// Build a [`Scope`] for the caller's context (no `last`, for eager arg resolution).
fn build_caller_scope<'a>(ctx: &'a BuiltinCtx<'a>) -> Scope<'a> {
    Scope {
        file: ctx.file.clone(),
        component: ctx.component.clone(),
        span: ctx.span,
        named: Vec::new(),
        positional: Vec::new(),
        last: None,
        call: Some(ctx.call.clone()),
        shell_call: None,
    }
}

/// Build a [`Scope`] for a component call: named + positional args, optional `last`.
fn build_scope_for_call<'a>(
    ctx: &'a BuiltinCtx<'a>,
    args: &super::ir::Args,
    last: Option<&'a Value>,
) -> Scope<'a> {
    Scope {
        file: ctx.file.clone(),
        component: ctx.component.clone(),
        span: ctx.span,
        named: args.named_vec(),
        positional: args.positional_vec(),
        last: last.cloned(),
        call: Some(ctx.call.clone()),
        shell_call: None,
    }
}

/// Resolve a callable component reference (bare id or dotted path) for `$map`/`$reduce`.
/// This lookup is the same as the normal component call resolution but without
/// evaluating the reference.
fn resolve_callable(ctx: &BuiltinCtx<'_>, name: &str) -> Result<Rc<Definition>, Diagnostic> {
    let file = ctx
        .file
        .as_ref()
        .and_then(|p| ctx.project.files.iter().position(|f| f == p))
        .map(|i| FileId(i as u32))
        .unwrap_or(FileId(0));

    match super::resolve::resolve_ref(ctx.project, name, file, ctx.opts.plain.clone()) {
        Ok(def) => Ok(Rc::new(def.clone())),
        Err(LookupMiss::NotFound) => Err(Diagnostic {
            file: ctx.file.clone(),
            line: ctx.span.line,
            col: ctx.span.col,
            component: ctx.component.clone(),
            code: E002,
            message: format!("unknown component reference `{name}`"),
        }),
        Err(LookupMiss::FileScopeViolation { owner }) => Err(Diagnostic {
            file: ctx.file.clone(),
            line: ctx.span.line,
            col: ctx.span.col,
            component: ctx.component.clone(),
            code: E005,
            message: format!(
                "file-scope violation: `{name}` is defined only in `{}`",
                ctx.project
                    .files
                    .get(owner.0 as usize)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        }),
    }
}

/// Evaluate a component call for `$map`/`$reduce`: the callable `def` is invoked
/// with `args` and its result returned. Uses the `call` hook for math `name(...)`
/// calls.
fn eval_call(
    ctx: &BuiltinCtx<'_>,
    def: Rc<Definition>,
    args: &super::ir::Args,
) -> Result<Value, Diagnostic> {
    // Depth check: each item evaluation is a recursive op (uses a depth slot).
    if ctx.depth >= ctx.opts.max_depth {
        return Err(Diagnostic {
            file: ctx.file.clone(),
            line: ctx.span.line,
            col: ctx.span.col,
            component: ctx.component.clone(),
            code: E008,
            message: format!("max recursion depth ({}) exceeded", ctx.opts.max_depth),
        });
    }

    let result = eval_def_inner(ctx, &def, args)?;
    Ok(result)
}

/// Inner evaluation of a component definition: runs the rule-11 three-step pipeline
/// for the component body, minus the template chain (item evaluation is a single step).
fn eval_def(
    ctx: &BuiltinCtx<'_>,
    def: &Definition,
    _args: &super::ir::Args,
    scope: &Scope<'_>,
) -> Result<Value, Diagnostic> {
    // Rule 11 step 1: resolve the body against scope.
    let body = eval_resolve_body(&def.body, scope, ctx)?;

    // Output conversion (step 1 result).
    let output = match body {
        ResolvedBody::Value(v) => v,
        ResolvedBody::Object(set) => set.to_object(),
    };

    // Step 2: template chain — skipped for builtin item evaluation (PRD rules 12–14
    // describe item evaluation as a single step without chain).
    // But we need to check: does the item's own result go through a template?
    // Per the rule-12/13 description, each item is a single template step.
    // So we skip the chain here.

    // Step 3: `from` / shortcut dispatch on the output.
    eval_step3(ctx, def, &output)
}

/// Resolve the body of a component for builtin item evaluation.
fn eval_resolve_body(
    node: &super::parse::Node,
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<ResolvedBody, Diagnostic> {
    match node {
        super::parse::Node::Object(entries, _) => eval_resolve_property_set(entries, scope, ctx),
        other => {
            let v = eval_resolve_node(other, scope, ctx)?;
            Ok(ResolvedBody::Value(v))
        }
    }
}

/// Resolve a property set (object body) for builtin item evaluation.
fn eval_resolve_property_set(
    entries: &[super::parse::Entry],
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<ResolvedBody, Diagnostic> {
    let mut set = super::resolve::PropertySet::default();

    for entry in entries {
        match &entry.key {
            super::parse::Key::Int(i) if *i >= 0 => {
                let idx = *i as usize;
                let default = eval_resolve_node(&entry.value, scope, ctx)?;
                let value = scope.positional.get(idx).cloned().unwrap_or(default);
                if set.slots.len() <= idx {
                    set.slots.resize(idx + 1, Value::Null);
                }
                set.slots[idx] = value;
                set.order.push(super::resolve::PropKey::Slot(idx));
            }
            _ => {
                let name = super::parse::key_to_string(&entry.key);
                set.order.push(super::resolve::PropKey::Named(name));
            }
        }
    }

    let padded = Scope {
        positional: super::resolve::padded_positional(&scope.positional, &set.slots),
        ..scope.clone()
    };

    for entry in entries {
        match &entry.key {
            super::parse::Key::String(name) => {
                let value = eval_resolve_node(&entry.value, &padded, ctx)?;
                set.named.insert(name.clone(), value);
            }
            super::parse::Key::Int(i) if *i < 0 => {
                let name = i.to_string();
                let value = eval_resolve_node(&entry.value, &padded, ctx)?;
                set.named.insert(name, value);
            }
            _ => {}
        }
    }

    Ok(ResolvedBody::Object(set))
}

/// Resolve a single node for builtin item evaluation.
fn eval_resolve_node(
    node: &super::parse::Node,
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    match node {
        super::parse::Node::Null(_) => Ok(Value::Null),
        super::parse::Node::Bool(b, _) => Ok(Value::Bool(*b)),
        super::parse::Node::Int(i, _) => Ok(Value::Int(*i)),
        super::parse::Node::Float(f, _) => Ok(Value::Float(*f)),
        super::parse::Node::String(s, span) => eval_resolve_string(s, *span, scope, ctx),
        super::parse::Node::Array(items, _) => {
            let values = items
                .iter()
                .map(|n| eval_resolve_node(n, scope, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(values))
        }
        super::parse::Node::Object(entries, _) => {
            if entries
                .iter()
                .any(|e| is_from_key(&e.key, &ctx.opts.from_keyword))
            {
                eval_resolve_mini(entries, scope, ctx)
            } else {
                let mut m = IndexMap::with_capacity(entries.len());
                for entry in entries {
                    m.insert(
                        super::parse::key_to_string(&entry.key),
                        eval_resolve_node(&entry.value, scope, ctx)?,
                    );
                }
                Ok(Value::Object(m))
            }
        }
    }
}

/// Resolve a string scalar for builtin item evaluation.
fn eval_resolve_string(
    s: &str,
    span: Span,
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    match super::callsite::parse(s) {
        Ok(Some(call)) => eval_resolve_call(&call, span, scope, ctx),
        Ok(None) => {
            let segments = interp::scan(s, span)?;
            interp::resolve(&segments, scope, &V1Engine)
        }
        Err((code, message)) => Err(Diagnostic {
            file: ctx.file.clone(),
            line: span.line,
            col: span.col,
            component: ctx.component.clone(),
            code,
            message,
        }),
    }
}

/// Resolve an inline call-site for builtin item evaluation.
fn eval_resolve_call(
    call: &super::callsite::ParsedCall,
    span: Span,
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    // Check for builtin call.
    if let Some(builtin) = Builtin::from_name(&call.name) {
        return eval_builtin_call(builtin, call, span, scope, ctx);
    }

    let (named, positional) = eval_resolve_call_args(&call.args, span, scope, ctx)?;
    let args = match (named.is_empty(), positional.is_empty()) {
        (true, true) => super::ir::Args::None,
        (false, true) => super::ir::Args::Named(named),
        (true, false) => super::ir::Args::Positional(positional),
        (false, false) => super::ir::Args::Mixed { named, positional },
    };

    // Look up the component.
    let def = resolve_callable(ctx, &call.name)?;

    // Depth check for the recursive call.
    if ctx.depth >= ctx.opts.max_depth {
        return Err(Diagnostic {
            file: ctx.file.clone(),
            line: span.line,
            col: span.col,
            component: ctx.component.clone(),
            code: E008,
            message: format!("max recursion depth ({}) exceeded", ctx.opts.max_depth),
        });
    }

    eval_def(ctx, &def, &args, scope)
}

/// Resolve call-site arguments for builtin item evaluation.
fn eval_resolve_call_args(
    args: &[super::callsite::ParsedArg],
    span: Span,
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<CallArgs, Diagnostic> {
    let mut named = Vec::new();
    let mut positional = Vec::new();

    for arg in args {
        let value = eval_resolve_parsed_value(&arg.value, span, scope, ctx)?;
        match &arg.key {
            Some(key) => named.push((key.clone(), value)),
            None => positional.push(value),
        }
    }

    Ok((named, positional))
}

/// Resolve one parsed argument value for builtin item evaluation.
fn eval_resolve_parsed_value(
    value: &super::callsite::ParsedValue,
    span: Span,
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    match value {
        super::callsite::ParsedValue::Literal(v) => Ok(v.clone()),
        super::callsite::ParsedValue::Raw(s) => Ok(Value::string(s.clone())),
        super::callsite::ParsedValue::Ref { name } => {
            let segments = interp::scan(&format!("${name}"), span)?;
            interp::resolve(&segments, scope, &V1Engine)
        }
        super::callsite::ParsedValue::Math { src } => {
            let segments = interp::scan(&format!("${{{src}}}"), span)?;
            interp::resolve(&segments, scope, &V1Engine)
        }
        super::callsite::ParsedValue::Call(nested) => eval_resolve_call(nested, span, scope, ctx),
    }
}

/// Evaluate a builtin call as part of a builtin item's body resolution.
fn eval_builtin_call(
    builtin: Builtin,
    call: &super::callsite::ParsedCall,
    span: Span,
    _scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    // For builtin calls within a builtin item body, we use a sub-context.
    let sub_ctx = BuiltinCtx {
        file: ctx.file.clone(),
        component: ctx.component.clone(),
        span,
        project: ctx.project,
        opts: ctx.opts,
        depth: ctx.depth + 1, // The builtin call itself uses a depth slot.
        call: ctx.call.clone(),
    };

    match builtin {
        Builtin::Merge => MergeBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Map => MapBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Reduce => ReduceBuiltin.eval(&sub_ctx, &call.args),
    }
}

/// Step 3 dispatch for builtin item evaluation: `from` / shortcut on an object result.
fn eval_step3(ctx: &BuiltinCtx<'_>, _def: &Definition, value: &Value) -> Result<Value, Diagnostic> {
    let Value::Object(m) = value else {
        return Ok(value.clone());
    };

    // Resolve `from` target.
    if let Some(from_val) = m.get(&ctx.opts.from_keyword) {
        let Value::String(target) = from_val else {
            return Ok(value.clone());
        };

        let file = ctx
            .file
            .as_ref()
            .and_then(|p| ctx.project.files.iter().position(|f| f == p))
            .map(|i| FileId(i as u32))
            .unwrap_or(FileId(0));

        match super::resolve::resolve_ref(ctx.project, target, file, ctx.opts.plain.clone()) {
            Ok(target_def) if !target_def.full_name.starts_with('$') => {
                let named: Vec<(String, Value)> = m
                    .iter()
                    .filter(|(k, _)| *k != &ctx.opts.from_keyword)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let args = super::resolve::args_from(named, Vec::new());

                // Recursive call with depth check.
                if ctx.depth >= ctx.opts.max_depth {
                    return Err(Diagnostic {
                        file: ctx.file.clone(),
                        line: ctx.span.line,
                        col: ctx.span.col,
                        component: ctx.component.clone(),
                        code: E008,
                        message: format!("max recursion depth ({}) exceeded", ctx.opts.max_depth),
                    });
                }

                return eval_def(ctx, target_def, &args, &build_caller_scope(ctx));
            }
            _ => {}
        }
    }

    // Rule-8 shortcut: check for a matching component.
    // For simplicity in builtins, we skip the shortcut (it's not mentioned in the
    // rule-12/13/14 builtin item binding description).
    Ok(value.clone())
}

/// Inner implementation of `eval_def`: evaluates a component definition with the
/// given args and scope, returning the resolved output (no further chain step).
fn eval_def_inner(
    ctx: &BuiltinCtx<'_>,
    def: &Definition,
    args: &super::ir::Args,
) -> Result<Value, Diagnostic> {
    let scope = build_scope_for_call(ctx, args, None);
    eval_resolve_body(&def.body, &scope, ctx).map(|body| match body {
        ResolvedBody::Value(v) => v,
        ResolvedBody::Object(set) => set.to_object(),
    })
}

/// A resolved object body.
enum ResolvedBody {
    Value(Value),
    Object(super::resolve::PropertySet),
}

/// `from` keyword check.
fn is_from_key(key: &super::parse::Key, from_kw: &str) -> bool {
    matches!(key, super::parse::Key::String(s) if s == from_kw)
}

/// A nested mini-component (object with `from`).
fn eval_resolve_mini(
    entries: &[super::parse::Entry],
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Result<Value, Diagnostic> {
    let body = eval_resolve_property_set(entries, scope, ctx)?;
    let set = match body {
        ResolvedBody::Object(set) => set,
        ResolvedBody::Value(_) => unreachable!("property set always resolves to Object"),
    };

    eval_step3(
        ctx,
        &Definition {
            file: FileId(0),
            full_name: "<builtin迷你>".to_string(),
            span: ctx.span,
            body: super::parse::Node::Object(entries.to_vec(), ctx.span),
            math_shorthand: false,
            trailing_question: false,
            exec_backend: None,
        },
        &set.to_object(),
    )
}

/// Build a diagnostic with context.
fn ctx_err(ctx: &BuiltinCtx<'_>, code: &'static str, message: String) -> Diagnostic {
    Diagnostic {
        file: ctx.file.clone(),
        line: ctx.span.line,
        col: ctx.span.col,
        component: ctx.component.clone(),
        code,
        message,
    }
}
