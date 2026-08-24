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

use crate::diag::{Diagnostic, FileId, Span, E002, E005, E008, E010, E011};
use crate::interp;
use crate::ir::Value;
use crate::math::{CallHook, Scope, V1Engine};
use crate::namespace::Definition;
use crate::project::Options;
use crate::project::Project;
use crate::resolve::LookupMiss;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    // Existing
    Merge,
    Map,
    Reduce,
    // String
    Split,
    Join,
    Trim,
    Upper,
    Lower,
    Replace,
    // Array
    Filter,
    Sort,
    Reverse,
    Unique,
    Flatten,
    First,
    Last,
    Slice,
    // Object
    Keys,
    Values,
    Entries,
    FromEntries,
    Pick,
    Omit,
    // Type
    Type,
    IsArray,
    IsObject,
    IsString,
    IsNumber,
    IsNull,
    ToString,
    ToNumber,
    Coalesce,
    // Math aggregates
    Sum,
    Avg,
    Min,
    Max,
    // Conditional
    If,
    When,
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
            // String
            "split" => Some(Builtin::Split),
            "join" => Some(Builtin::Join),
            "trim" => Some(Builtin::Trim),
            "upper" => Some(Builtin::Upper),
            "lower" => Some(Builtin::Lower),
            "replace" => Some(Builtin::Replace),
            // Array
            "filter" => Some(Builtin::Filter),
            "sort" => Some(Builtin::Sort),
            "reverse" => Some(Builtin::Reverse),
            "unique" => Some(Builtin::Unique),
            "flatten" => Some(Builtin::Flatten),
            "first" => Some(Builtin::First),
            "last" => Some(Builtin::Last),
            "slice" => Some(Builtin::Slice),
            // Object
            "keys" => Some(Builtin::Keys),
            "values" => Some(Builtin::Values),
            "entries" => Some(Builtin::Entries),
            "from_entries" => Some(Builtin::FromEntries),
            "pick" => Some(Builtin::Pick),
            "omit" => Some(Builtin::Omit),
            // Type
            "type" => Some(Builtin::Type),
            "is_array" => Some(Builtin::IsArray),
            "is_object" => Some(Builtin::IsObject),
            "is_string" => Some(Builtin::IsString),
            "is_number" => Some(Builtin::IsNumber),
            "is_null" => Some(Builtin::IsNull),
            "to_string" => Some(Builtin::ToString),
            "to_number" => Some(Builtin::ToNumber),
            "coalesce" => Some(Builtin::Coalesce),
            // Math aggregates
            "sum" => Some(Builtin::Sum),
            "avg" => Some(Builtin::Avg),
            "min" => Some(Builtin::Min),
            "max" => Some(Builtin::Max),
            // Conditional
            "if" => Some(Builtin::If),
            "when" => Some(Builtin::When),
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

        let def = resolve_callable(ctx, &fn_name)?;

        let item_to_args = |item: &Value| -> Result<super::ir::Args, Diagnostic> {
            match item {
                Value::Object(m) => {
                    Ok(super::ir::Args::Named(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
                }
                v if v.is_scalar() => Ok(super::ir::Args::Positional(vec![v.clone()])),
                Value::Array(_) => Err(ctx_err(
                    ctx,
                    E011,
                    "array item in $reduce argument is not supported (array items must be objects or scalars)"
                        .to_string(),
                )),
                _ => unreachable!(),
            }
        };

        let source_name: Option<&str> = match &args[1].value {
            super::callsite::ParsedValue::Call(nested) if nested.args.is_empty() => {
                Some(nested.name.as_str())
            }
            super::callsite::ParsedValue::Math { src } => {
                src.strip_suffix("()").filter(|s| !s.is_empty())
            }
            _ => None,
        };

        if let Some(source_name) = source_name {
            if let Ok(source_def) = resolve_callable(ctx, source_name) {
                if let super::parse::Node::Array(items, _) = &source_def.body {
                    if items.is_empty() {
                        return Ok(Value::Null);
                    }

                    let mut prev: Option<Value> = None;
                    for item in items {
                        let last = prev.clone().or_else(|| init.clone());
                        let mut item_scope = build_caller_scope(ctx);
                        item_scope.last = last.clone();
                        let resolved_item = eval_resolve_node(item, &item_scope, ctx)?;
                        let item_args = item_to_args(&resolved_item)?;
                        let scope = build_scope_for_call(ctx, &item_args, last.as_ref());
                        let result = eval_def(ctx, &def, &item_args, &scope)?;
                        prev = Some(result);
                    }

                    return Ok(prev.expect("non-empty reduce always produces a result"));
                }
            }
        }

        // Fallback: second arg is eagerly evaluated, must be Array.
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

        // Single-element: run one step, `last` NOT in scope.
        if items.len() == 1 {
            let item = &items[0];
            let item_args = item_to_args(item)?;

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
            let item_args = item_to_args(&item)?;

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

// ---- $split ----

pub struct SplitBuiltin;

impl BuiltinImpl for SplitBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$split expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        let s = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::String(s) = s else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$split first argument must be a String, got {:?}", s),
            ));
        };

        let delim = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::String(delim) = delim else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$split second argument must be a String, got {:?}", delim),
            ));
        };

        if delim.is_empty() {
            return Err(ctx_err(
                ctx,
                E011,
                "$split: empty delimiter is not allowed".to_string(),
            ));
        }

        let parts: Vec<Value> = s.split(&delim).map(Value::string).collect();
        Ok(Value::Array(parts))
    }
}

// ---- $join ----

pub struct JoinBuiltin;

impl BuiltinImpl for JoinBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$join expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$join first argument must be an Array, got {:?}", arr),
            ));
        };

        let delim = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::String(delim) = delim else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$join second argument must be a String, got {:?}", delim),
            ));
        };

        let mut parts = Vec::with_capacity(items.len());
        for item in &items {
            let s = value_to_string(item).map_err(|_| {
                ctx_err(
                    ctx,
                    E011,
                    format!("$join: cannot coerce {:?} to string", item),
                )
            })?;
            parts.push(s);
        }

        Ok(Value::string(parts.join(&delim)))
    }
}

// ---- $trim ----

pub struct TrimBuiltin;

impl BuiltinImpl for TrimBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$trim expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let s = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::String(s) = s else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$trim argument must be a String, got {:?}", s),
            ));
        };

        Ok(Value::string(s.trim().to_string()))
    }
}

// ---- $upper ----

pub struct UpperBuiltin;

impl BuiltinImpl for UpperBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$upper expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let s = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::String(s) = s else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$upper argument must be a String, got {:?}", s),
            ));
        };

        Ok(Value::string(s.to_uppercase()))
    }
}

// ---- $lower ----

pub struct LowerBuiltin;

impl BuiltinImpl for LowerBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$lower expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let s = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::String(s) = s else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$lower argument must be a String, got {:?}", s),
            ));
        };

        Ok(Value::string(s.to_lowercase()))
    }
}

// ---- $replace ----

pub struct ReplaceBuiltin;

impl BuiltinImpl for ReplaceBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 3 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$replace expects exactly 3 arguments, got {}", args.len()),
            ));
        }

        let s = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::String(s) = s else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$replace first argument must be a String, got {:?}", s),
            ));
        };

        let pattern = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::String(pattern) = pattern else {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$replace second argument must be a String, got {:?}",
                    pattern
                ),
            ));
        };

        let replacement = resolve_parsed_value(&args[2].value, ctx)?;
        let Value::String(replacement) = replacement else {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$replace third argument must be a String, got {:?}",
                    replacement
                ),
            ));
        };

        if pattern.is_empty() {
            return Err(ctx_err(
                ctx,
                E011,
                "$replace: empty pattern is not allowed".to_string(),
            ));
        }

        Ok(Value::string(s.replace(&pattern, &replacement)))
    }
}

// ---- $first ----

pub struct FirstBuiltin;

impl BuiltinImpl for FirstBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$first expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$first argument must be an Array, got {:?}", arr),
            ));
        };

        Ok(items.into_iter().next().unwrap_or(Value::Null))
    }
}

// ---- $last ----

pub struct LastBuiltin;

impl BuiltinImpl for LastBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$last expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$last argument must be an Array, got {:?}", arr),
            ));
        };

        Ok(items.into_iter().last().unwrap_or(Value::Null))
    }
}

// ---- $sort ----

pub struct SortBuiltin;

impl BuiltinImpl for SortBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$sort expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$sort argument must be an Array, got {:?}", arr),
            ));
        };

        if items.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        // Determine the element kind from the first element.
        let all_int = items.iter().all(|v| matches!(v, Value::Int(_)));
        let all_float = items.iter().all(|v| matches!(v, Value::Float(_)));
        let all_string = items.iter().all(|v| matches!(v, Value::String(_)));

        if all_int {
            let mut nums: Vec<i64> = items
                .iter()
                .filter_map(|v| match v {
                    Value::Int(i) => Some(*i),
                    _ => unreachable!(),
                })
                .collect();
            nums.sort();
            Ok(Value::Array(nums.into_iter().map(Value::Int).collect()))
        } else if all_float {
            let mut nums: Vec<f64> = items
                .iter()
                .filter_map(|v| match v {
                    Value::Float(f) => Some(*f),
                    _ => unreachable!(),
                })
                .collect();
            nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Ok(Value::Array(nums.into_iter().map(Value::Float).collect()))
        } else if all_string {
            let mut strs: Vec<String> = items
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => unreachable!(),
                })
                .collect();
            strs.sort();
            Ok(Value::Array(strs.into_iter().map(Value::String).collect()))
        } else {
            Err(ctx_err(
                ctx,
                E011,
                "$sort requires all elements to be the same type (all Int, all Float, or all String)"
                    .to_string(),
            ))
        }
    }
}

// ---- $reverse ----

pub struct ReverseBuiltin;

impl BuiltinImpl for ReverseBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$reverse expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$reverse argument must be an Array, got {:?}", arr),
            ));
        };

        let mut reversed = items;
        reversed.reverse();
        Ok(Value::Array(reversed))
    }
}

// ---- $unique ----

pub struct UniqueBuiltin;

impl BuiltinImpl for UniqueBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$unique expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$unique argument must be an Array, got {:?}", arr),
            ));
        };

        let mut seen: Vec<Value> = Vec::new();
        for item in items {
            if !seen.iter().any(|s| s == &item) {
                seen.push(item);
            }
        }
        Ok(Value::Array(seen))
    }
}

// ---- $flatten ----

pub struct FlattenBuiltin;

impl BuiltinImpl for FlattenBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$flatten expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$flatten argument must be an Array, got {:?}", arr),
            ));
        };

        let mut result = Vec::new();
        for item in items {
            match item {
                Value::Array(inner) => result.extend(inner),
                other => result.push(other),
            }
        }
        Ok(Value::Array(result))
    }
}

// ---- $slice ----

pub struct SliceBuiltin;

impl BuiltinImpl for SliceBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 3 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$slice expects exactly 3 arguments, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$slice first argument must be an Array, got {:?}", arr),
            ));
        };

        let start_val = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Int(start_raw) = start_val else {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$slice second argument (start) must be an Int, got {:?}",
                    start_val
                ),
            ));
        };

        let end_val = resolve_parsed_value(&args[2].value, ctx)?;
        let Value::Int(end_raw) = end_val else {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$slice third argument (end) must be an Int, got {:?}",
                    end_val
                ),
            ));
        };

        let len = items.len() as i64;

        // Normalize negative indices.
        let start = if start_raw < 0 {
            (len + start_raw).max(0) as usize
        } else {
            start_raw.min(len) as usize
        };

        let end = if end_raw < 0 {
            (len + end_raw).max(0) as usize
        } else {
            end_raw.min(len) as usize
        };

        // Ensure start <= end (swap if needed is not spec'd; clamp instead).
        let (start, end) = if start > end {
            (end, start)
        } else {
            (start, end)
        };

        Ok(Value::Array(items[start..end].to_vec()))
    }
}

// ---- $filter ----

pub struct FilterBuiltin;

impl BuiltinImpl for FilterBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$filter expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        // First arg: unevaluated callable component reference.
        let fn_name = match &args[0].value {
            super::callsite::ParsedValue::Ref { name } => name.clone(),
            other => {
                return Err(ctx_err(ctx, E011,
                    format!("first argument of $filter must be an unevaluated callable component reference, got {:?}", other)));
            }
        };

        // Second arg: eagerly evaluated, must be Array.
        let arr = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("second argument of $filter must be an Array, got {:?}", arr),
            ));
        };

        // Empty array → empty array.
        if items.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        // Look up the callable component.
        let def = resolve_callable(ctx, &fn_name)?;

        let mut out = Vec::new();
        for item in items {
            let item_args = match &item {
                Value::Object(m) => {
                    super::ir::Args::Named(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                }
                v if v.is_scalar() => super::ir::Args::Positional(vec![v.clone()]),
                Value::Array(_) => {
                    return Err(ctx_err(ctx, E011,
                        "array item in $filter argument is not supported (array items must be objects or scalars)".to_string()));
                }
                _ => unreachable!("Value::Object and scalar cover all non-array cases"),
            };

            let result = eval_call(ctx, def.clone(), &item_args)?;
            if is_truthy(&result) {
                out.push(item);
            }
        }

        Ok(Value::Array(out))
    }
}

// ---- $keys ----

pub struct KeysBuiltin;

impl BuiltinImpl for KeysBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$keys expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let obj = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Object(m) = obj else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$keys argument must be an Object, got {:?}", obj),
            ));
        };

        Ok(Value::Array(
            m.keys().map(|k| Value::string(k.clone())).collect(),
        ))
    }
}

// ---- $values ----

pub struct ValuesBuiltin;

impl BuiltinImpl for ValuesBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$values expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let obj = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Object(m) = obj else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$values argument must be an Object, got {:?}", obj),
            ));
        };

        Ok(Value::Array(m.values().cloned().collect()))
    }
}

// ---- $entries ----

pub struct EntriesBuiltin;

impl BuiltinImpl for EntriesBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$entries expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let obj = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Object(m) = obj else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$entries argument must be an Object, got {:?}", obj),
            ));
        };

        let entries: Vec<Value> = m
            .iter()
            .map(|(k, v)| {
                let mut pair = IndexMap::with_capacity(2);
                pair.insert("key".to_string(), Value::string(k.clone()));
                pair.insert("value".to_string(), v.clone());
                Value::Object(pair)
            })
            .collect();

        Ok(Value::Array(entries))
    }
}

// ---- $from_entries ----

pub struct FromEntriesBuiltin;

impl BuiltinImpl for FromEntriesBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$from_entries expects exactly 1 argument, got {}",
                    args.len()
                ),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$from_entries argument must be an Array, got {:?}", arr),
            ));
        };

        let mut result = IndexMap::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            let Value::Object(entry) = item else {
                return Err(ctx_err(
                    ctx,
                    E011,
                    format!("$from_entries: element at index {} is not an Object", i),
                ));
            };

            let Some(Value::String(key)) = entry.get("key") else {
                return Err(ctx_err(
                    ctx,
                    E011,
                    format!(
                        "$from_entries: element at index {} is missing a string `key` field",
                        i
                    ),
                ));
            };

            let value = entry.get("value").cloned().unwrap_or(Value::Null);
            result.insert(key.clone(), value);
        }

        Ok(Value::Object(result))
    }
}

// ---- $pick ----

pub struct PickBuiltin;

impl BuiltinImpl for PickBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$pick expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        let obj = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Object(m) = obj else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$pick first argument must be an Object, got {:?}", obj),
            ));
        };

        let keys = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Array(key_items) = keys else {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$pick second argument must be an Array of strings, got {:?}",
                    keys
                ),
            ));
        };

        let mut result = IndexMap::with_capacity(key_items.len());
        for item in key_items {
            let Value::String(k) = item else {
                return Err(ctx_err(
                    ctx,
                    E011,
                    format!("$pick: key list contains a non-string element {:?}", item),
                ));
            };
            if let Some(v) = m.get(&k) {
                result.insert(k, v.clone());
            }
        }

        Ok(Value::Object(result))
    }
}

// ---- $omit ----

pub struct OmitBuiltin;

impl BuiltinImpl for OmitBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$omit expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        let obj = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Object(m) = obj else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$omit first argument must be an Object, got {:?}", obj),
            ));
        };

        let keys = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Array(key_items) = keys else {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$omit second argument must be an Array of strings, got {:?}",
                    keys
                ),
            ));
        };

        let mut omit_set = Vec::with_capacity(key_items.len());
        for item in &key_items {
            let Value::String(k) = item else {
                return Err(ctx_err(
                    ctx,
                    E011,
                    format!("$omit: key list contains a non-string element {:?}", item),
                ));
            };
            omit_set.push(k.clone());
        }

        let mut result = IndexMap::with_capacity(m.len());
        for (k, v) in m {
            if !omit_set.contains(&k) {
                result.insert(k.clone(), v.clone());
            }
        }

        Ok(Value::Object(result))
    }
}

// ---- $type ----

pub struct TypeBuiltin;

impl BuiltinImpl for TypeBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$type expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        let type_name = match &val {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        Ok(Value::string(type_name.to_string()))
    }
}

// ---- $is_array ----

pub struct IsArrayBuiltin;

impl BuiltinImpl for IsArrayBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$is_array expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        Ok(Value::Bool(matches!(val, Value::Array(_))))
    }
}

// ---- $is_object ----

pub struct IsObjectBuiltin;

impl BuiltinImpl for IsObjectBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$is_object expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        Ok(Value::Bool(matches!(val, Value::Object(_))))
    }
}

// ---- $is_string ----

pub struct IsStringBuiltin;

impl BuiltinImpl for IsStringBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$is_string expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        Ok(Value::Bool(matches!(val, Value::String(_))))
    }
}

// ---- $is_number ----

pub struct IsNumberBuiltin;

impl BuiltinImpl for IsNumberBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$is_number expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        Ok(Value::Bool(matches!(val, Value::Int(_) | Value::Float(_))))
    }
}

// ---- $is_null ----

pub struct IsNullBuiltin;

impl BuiltinImpl for IsNullBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$is_null expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        Ok(Value::Bool(matches!(val, Value::Null)))
    }
}

// ---- $to_string ----

pub struct ToStringBuiltin;

impl BuiltinImpl for ToStringBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$to_string expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        let s = value_to_string(&val).map_err(|_| {
            ctx_err(
                ctx,
                E011,
                format!("$to_string: cannot coerce {:?} to string", val),
            )
        })?;
        Ok(Value::string(s))
    }
}

// ---- $to_number ----

pub struct ToNumberBuiltin;

impl BuiltinImpl for ToNumberBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$to_number expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let val = resolve_parsed_value(&args[0].value, ctx)?;
        match &val {
            Value::String(s) => {
                // Try Int first, then Float; if neither → Null.
                if let Ok(i) = s.parse::<i64>() {
                    Ok(Value::Int(i))
                } else if let Ok(f) = s.parse::<f64>() {
                    Ok(Value::Float(f))
                } else {
                    Ok(Value::Null)
                }
            }
            Value::Int(_) | Value::Float(_) => Ok(val),
            _ => Ok(Value::Null),
        }
    }
}

// ---- $coalesce ----

pub struct CoalesceBuiltin;

impl BuiltinImpl for CoalesceBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        // Variadic: accepts 0+ args, all evaluated eagerly.
        for arg in args {
            let val = resolve_parsed_value(&arg.value, ctx)?;
            if !matches!(val, Value::Null) {
                return Ok(val);
            }
        }
        Ok(Value::Null)
    }
}

// ---- $sum ----

pub struct SumBuiltin;

impl BuiltinImpl for SumBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$sum expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$sum argument must be an Array, got {:?}", arr),
            ));
        };

        if items.is_empty() {
            return Ok(Value::Int(0));
        }

        let mut has_float = false;
        let mut int_sum: i64 = 0;
        let mut float_sum: f64 = 0.0;

        for item in &items {
            match item {
                Value::Int(i) => {
                    if has_float {
                        float_sum += *i as f64;
                    } else {
                        int_sum += i;
                    }
                }
                Value::Float(f) => {
                    if !has_float {
                        float_sum = int_sum as f64;
                        has_float = true;
                    }
                    float_sum += f;
                }
                other => {
                    return Err(ctx_err(
                        ctx,
                        E011,
                        format!("$sum: non-numeric element {:?}", other),
                    ));
                }
            }
        }

        if has_float {
            Ok(Value::Float(float_sum))
        } else {
            Ok(Value::Int(int_sum))
        }
    }
}

// ---- $avg ----

pub struct AvgBuiltin;

impl BuiltinImpl for AvgBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$avg expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$avg argument must be an Array, got {:?}", arr),
            ));
        };

        if items.is_empty() {
            return Ok(Value::Null);
        }

        let mut sum: f64 = 0.0;

        for item in &items {
            match item {
                Value::Int(i) => {
                    sum += *i as f64;
                }
                Value::Float(f) => {
                    sum += f;
                }
                other => {
                    return Err(ctx_err(
                        ctx,
                        E011,
                        format!("$avg: non-numeric element {:?}", other),
                    ));
                }
            }
        }

        Ok(Value::Float(sum / items.len() as f64))
    }
}

// ---- $min ----

pub struct MinBuiltin;

impl BuiltinImpl for MinBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$min expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$min argument must be an Array, got {:?}", arr),
            ));
        };

        if items.is_empty() {
            return Ok(Value::Null);
        }

        let mut min_item = &items[0];
        let mut min_float = value_to_f64(min_item).ok_or_else(|| {
            ctx_err(
                ctx,
                E011,
                format!("$min: non-numeric element {:?}", min_item),
            )
        })?;

        for item in items.iter().skip(1) {
            let f = value_to_f64(item).ok_or_else(|| {
                ctx_err(ctx, E011, format!("$min: non-numeric element {:?}", item))
            })?;
            if f < min_float {
                min_float = f;
                min_item = item;
            }
        }

        Ok(min_item.clone())
    }
}

// ---- $max ----

pub struct MaxBuiltin;

impl BuiltinImpl for MaxBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 1 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$max expects exactly 1 argument, got {}", args.len()),
            ));
        }

        let arr = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$max argument must be an Array, got {:?}", arr),
            ));
        };

        if items.is_empty() {
            return Ok(Value::Null);
        }

        let mut max_item = &items[0];
        let mut max_float = value_to_f64(max_item).ok_or_else(|| {
            ctx_err(
                ctx,
                E011,
                format!("$max: non-numeric element {:?}", max_item),
            )
        })?;

        for item in items.iter().skip(1) {
            let f = value_to_f64(item).ok_or_else(|| {
                ctx_err(ctx, E011, format!("$max: non-numeric element {:?}", item))
            })?;
            if f > max_float {
                max_float = f;
                max_item = item;
            }
        }

        Ok(max_item.clone())
    }
}

// ---- $if (conditional — lazy branches) ----

pub struct IfBuiltin;

impl BuiltinImpl for IfBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 3 {
            return Err(ctx_err(
                ctx,
                E011,
                format!(
                    "$if expects exactly 3 arguments (cond, then, else), got {}",
                    args.len()
                ),
            ));
        }

        // Arg 0 (cond): evaluated eagerly, must be Bool.
        let cond = resolve_parsed_value(&args[0].value, ctx)?;
        let Value::Bool(b) = cond else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$if: condition must be a Bool, got {:?}", cond),
            ));
        };

        // Args 1 and 2 (then/else): resolved lazily — only the selected branch.
        if b {
            resolve_parsed_value(&args[1].value, ctx)
        } else {
            resolve_parsed_value(&args[2].value, ctx)
        }
    }
}

/// Handle the property-call form of `$if`:
/// ```yaml
/// if: <cond>
///   then: <then_val>
///   else: <else_val>
/// ```
/// Only `if`/`then`/`else` property keys allowed; any other key → E010.
/// Returns `Some(result)` if this is an `if/then/else` property object,
/// `None` otherwise (no `if`/`then`/`else` keys at all).
pub fn try_eval_if_property(
    entries: &[super::parse::Entry],
    scope: &Scope<'_>,
    ctx: &BuiltinCtx<'_>,
) -> Option<Result<Value, Diagnostic>> {
    let mut has_if_then_else = false;
    let mut if_entry = None;
    let mut then_entry = None;
    let mut else_entry = None;
    let mut unknown_entry = None;
    for entry in entries {
        let key = super::parse::key_to_string(&entry.key);
        match key.as_str() {
            "if" => {
                has_if_then_else = true;
                if_entry = Some(entry);
            }
            "then" => {
                has_if_then_else = true;
                then_entry = Some(entry);
            }
            "else" => {
                has_if_then_else = true;
                else_entry = Some(entry);
            }
            _ => {
                unknown_entry = Some(entry);
            }
        }
    }
    if !has_if_then_else {
        return None;
    }
    if let Some(entry) = unknown_entry {
        return Some(Err(Diagnostic {
            file: ctx.file.clone(),
            line: entry.key_span.line,
            col: entry.key_span.col,
            component: ctx.component.clone(),
            code: E010,
            message: format!(
                "$if property-call: unexpected key `{}` — only `if`, `then`, `else` are allowed",
                super::parse::key_to_string(&entry.key)
            ),
        }));
    }
    let (if_e, then_e, else_e) = match (if_entry, then_entry, else_entry) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return None,
    };

    // Resolve condition eagerly — must be Bool.
    let cond_result = eval_resolve_node(&if_e.value, scope, ctx);
    let cond = match cond_result {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Value::Bool(b) = cond else {
        return Some(Err(ctx_err(
            ctx,
            E011,
            format!("$if: condition must be a Bool, got {:?}", cond),
        )));
    };

    // Lazily resolve only the selected branch.
    if b {
        Some(eval_resolve_node(&then_e.value, scope, ctx))
    } else {
        Some(eval_resolve_node(&else_e.value, scope, ctx))
    }
}

// ---- $when (map + filter combined) ----

pub struct WhenBuiltin;

impl BuiltinImpl for WhenBuiltin {
    fn eval(
        &self,
        ctx: &BuiltinCtx<'_>,
        args: &[super::callsite::ParsedArg],
    ) -> Result<Value, Diagnostic> {
        if args.len() != 2 {
            return Err(ctx_err(
                ctx,
                E011,
                format!("$when expects exactly 2 arguments, got {}", args.len()),
            ));
        }

        // First arg: unevaluated callable component reference (same as $map/$filter).
        let fn_name = match &args[0].value {
            super::callsite::ParsedValue::Ref { name } => name.clone(),
            other => {
                return Err(ctx_err(ctx, E011,
                    format!("first argument of $when must be an unevaluated callable component reference, got {:?}", other)));
            }
        };

        // Second arg: eagerly evaluated, must be Array.
        let arr = resolve_parsed_value(&args[1].value, ctx)?;
        let Value::Array(items) = arr else {
            return Err(ctx_err(
                ctx,
                E011,
                format!("second argument of $when must be an Array, got {:?}", arr),
            ));
        };

        // Empty array → empty array.
        if items.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }

        // Look up the callable component.
        let def = resolve_callable(ctx, &fn_name)?;

        let mut out = Vec::new();
        for item in items {
            let item_args = match &item {
                Value::Object(m) => {
                    super::ir::Args::Named(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                }
                v if v.is_scalar() => super::ir::Args::Positional(vec![v.clone()]),
                Value::Array(_) => {
                    return Err(ctx_err(ctx, E011,
                        "array item in $when argument is not supported (array items must be objects or scalars)".to_string()));
                }
                _ => unreachable!("Value::Object and scalar cover all non-array cases"),
            };

            let result = eval_call(ctx, def.clone(), &item_args)?;
            if is_truthy(&result) {
                out.push(result);
            }
        }

        Ok(Value::Array(out))
    }
}

// ---- Shared helper functions ----

/// Check if a value is truthy: `Bool(true)` is truthy, `Bool(false)` and
/// `Null` are falsy; everything else (numbers, strings, arrays, objects) is truthy.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        _ => true,
    }
}

/// Extract the f64 value from a numeric `Value` (Int or Float).
fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// Render a [`Value`] to a string using the same rules as interpolation.
/// Int renders plainly, Float via `render_f64`, Bool as `"true"`/`"false"`,
/// Null as `"null"`, String passes through. Array/Object → `Err(NoStringRender)`.
fn value_to_string(v: &Value) -> Result<String, crate::ir::NoStringRender> {
    crate::ir::render_value(v)
}

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
            let mut resolved_args = Vec::with_capacity(nested.args.len());
            for arg in &nested.args {
                resolved_args.push(resolve_parsed_value(&arg.value, ctx)?);
            }
            (ctx.call)(&nested.name, &resolved_args)
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
    if def.math_shorthand {
        let super::parse::Node::String(math_src, span) = &def.body else {
            return Err(ctx_err(
                ctx,
                E010,
                "value for `$` component-name shorthand must be a string (math source)".to_string(),
            ));
        };
        let segments = interp::scan(&format!("${{{math_src}}}"), *span)?;
        return interp::resolve(&segments, scope, &V1Engine);
    }
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
        super::parse::Node::Object(entries, _) => {
            // Property-call form of `$if`: object with exactly keys `if`, `then`, `else`.
            if let Some(result) = try_eval_if_property(entries, scope, ctx) {
                return Ok(ResolvedBody::Value(result?));
            }
            eval_resolve_property_set(entries, scope, ctx)
        }
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
            // Property-call form of `$if`: object with exactly keys `if`, `then`, `else`.
            if let Some(result) = try_eval_if_property(entries, scope, ctx) {
                return result;
            }
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
        Builtin::Split => SplitBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Join => JoinBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Trim => TrimBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Upper => UpperBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Lower => LowerBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Replace => ReplaceBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Filter => FilterBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Sort => SortBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Reverse => ReverseBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Unique => UniqueBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Flatten => FlattenBuiltin.eval(&sub_ctx, &call.args),
        Builtin::First => FirstBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Last => LastBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Slice => SliceBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Keys => KeysBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Values => ValuesBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Entries => EntriesBuiltin.eval(&sub_ctx, &call.args),
        Builtin::FromEntries => FromEntriesBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Pick => PickBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Omit => OmitBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Type => TypeBuiltin.eval(&sub_ctx, &call.args),
        Builtin::IsArray => IsArrayBuiltin.eval(&sub_ctx, &call.args),
        Builtin::IsObject => IsObjectBuiltin.eval(&sub_ctx, &call.args),
        Builtin::IsString => IsStringBuiltin.eval(&sub_ctx, &call.args),
        Builtin::IsNumber => IsNumberBuiltin.eval(&sub_ctx, &call.args),
        Builtin::IsNull => IsNullBuiltin.eval(&sub_ctx, &call.args),
        Builtin::ToString => ToStringBuiltin.eval(&sub_ctx, &call.args),
        Builtin::ToNumber => ToNumberBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Coalesce => CoalesceBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Sum => SumBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Avg => AvgBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Min => MinBuiltin.eval(&sub_ctx, &call.args),
        Builtin::Max => MaxBuiltin.eval(&sub_ctx, &call.args),
        Builtin::If => IfBuiltin.eval(&sub_ctx, &call.args),
        Builtin::When => WhenBuiltin.eval(&sub_ctx, &call.args),
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
    if def.math_shorthand {
        let super::parse::Node::String(math_src, span) = &def.body else {
            return Err(ctx_err(
                ctx,
                E010,
                "value for `$` component-name shorthand must be a string (math source)".to_string(),
            ));
        };
        let segments = interp::scan(&format!("${{{math_src}}}"), *span)?;
        return interp::resolve(&segments, &scope, &V1Engine);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{E002, E011};
    use crate::ir::Value;
    use crate::project::Options;
    use crate::resolve::compile_component;
    use indexmap::IndexMap;
    use std::path::{Path, PathBuf};

    fn project_with(files: &[(&str, &str)]) -> crate::project::Project {
        let mut p = crate::project::Project::new();
        p.root = PathBuf::from("/proj");
        for (i, (path, src)) in files.iter().enumerate() {
            p.files.push(PathBuf::from("/proj").join(path));
            let node = crate::parse::parse_document(src).expect("parse fixture");
            let ex = crate::namespace::extract_document(FileId(i as u32), &node);
            let namespace = Path::new(path)
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .replace('/', ".");
            for def in ex.defs {
                p.namespaces.register(&namespace, def).unwrap();
            }
            for def in ex.file_scoped_defs {
                p.file_scoped.register(FileId(i as u32), def).unwrap();
            }
        }
        p
    }

    fn compile_ok(p: &crate::project::Project, component: &str, args: &crate::ir::Args) -> Value {
        compile_component(p, component, args, &Options::default())
            .unwrap_or_else(|ds| panic!("{component}: {}", ds[0].message))
    }

    fn compile_err(
        p: &crate::project::Project,
        component: &str,
        args: &crate::ir::Args,
    ) -> Diagnostic {
        compile_component(p, component, args, &Options::default())
            .unwrap_err()
            .into_iter()
            .next()
            .expect("at least one diagnostic")
    }

    // =====================================================
    // String builtins
    // =====================================================

    #[test]
    fn split_basic() {
        let p = project_with(&[("main.yml", "main: $split(\"a,b,c\", \",\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![
                Value::string("a"),
                Value::string("b"),
                Value::string("c")
            ])
        );
    }

    #[test]
    fn split_no_match() {
        let p = project_with(&[("main.yml", "main: $split(\"abc\", \",\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::string("abc")])
        );
    }

    #[test]
    fn split_multi_char_delim() {
        let p = project_with(&[("main.yml", "main: $split(\"a::b::c\", \"::\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![
                Value::string("a"),
                Value::string("b"),
                Value::string("c")
            ])
        );
    }

    #[test]
    fn split_empty_delim_is_e011() {
        let p = project_with(&[("main.yml", "main: $split(\"hello\", \"\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
        assert!(err.message.contains("empty delimiter"), "{}", err.message);
    }

    #[test]
    fn split_first_arg_not_string_is_e011() {
        let p = project_with(&[("main.yml", "main: $split(123, \",\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn split_second_arg_not_string_is_e011() {
        let p = project_with(&[("main.yml", "main: $split(\"abc\", 123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn join_basic() {
        let p = project_with(&[(
            "main.yml",
            "arr: [\"a\", \"b\", \"c\"]\nmain: $join(${arr()}, \"-\")\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("a-b-c")
        );
    }

    #[test]
    fn join_empty_array() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $join(${arr()}, \"x\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("")
        );
    }

    #[test]
    fn join_empty_delim() {
        let p = project_with(&[(
            "main.yml",
            "arr: [\"a\", \"b\"]\nmain: $join(${arr()}, \"\")\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("ab")
        );
    }

    #[test]
    fn join_first_arg_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $join(\"not an array\", \",\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn join_second_arg_not_string_is_e011() {
        let p = project_with(&[("main.yml", "arr: [1]\nmain: $join(${arr()}, 123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn join_non_string_elements_coerce() {
        let p = project_with(&[("main.yml", "arr: [1, 2, 3]\nmain: $join(${arr()}, \",\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("1,2,3")
        );
    }

    #[test]
    fn trim_basic() {
        let p = project_with(&[("main.yml", "main: $trim(\"  hello  \")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello")
        );
    }

    #[test]
    fn trim_no_whitespace() {
        let p = project_with(&[("main.yml", "main: $trim(\"hello\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello")
        );
    }

    #[test]
    fn trim_not_string_is_e011() {
        let p = project_with(&[("main.yml", "main: $trim(123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn upper_basic() {
        let p = project_with(&[("main.yml", "main: $upper(\"hello\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("HELLO")
        );
    }

    #[test]
    fn upper_not_string_is_e011() {
        let p = project_with(&[("main.yml", "main: $upper([])\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn lower_basic() {
        let p = project_with(&[("main.yml", "main: $lower(\"HELLO\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello")
        );
    }

    #[test]
    fn lower_not_string_is_e011() {
        let p = project_with(&[("main.yml", "main: $lower(123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn replace_basic() {
        let p = project_with(&[(
            "main.yml",
            "main: $replace(\"hello world\", \"world\", \"rust\")\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello rust")
        );
    }

    #[test]
    fn replace_all_occurrences() {
        let p = project_with(&[("main.yml", "main: $replace(\"aaaa\", \"a\", \"b\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("bbbb")
        );
    }

    #[test]
    fn replace_no_match() {
        let p = project_with(&[("main.yml", "main: $replace(\"abc\", \"x\", \"y\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("abc")
        );
    }

    #[test]
    fn replace_empty_pattern_is_e011() {
        let p = project_with(&[("main.yml", "main: $replace(\"abc\", \"\", \"y\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
        assert!(err.message.contains("empty pattern"), "{}", err.message);
    }

    #[test]
    fn replace_first_arg_not_string_is_e011() {
        let p = project_with(&[("main.yml", "main: $replace(123, \"a\", \"b\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    // =====================================================
    // Array builtins
    // =====================================================

    #[test]
    fn first_basic() {
        let p = project_with(&[("main.yml", "arr: [1, 2, 3]\nmain: $first(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(1)
        );
    }

    #[test]
    fn first_empty_array() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $first(${arr()})\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn first_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $first(\"not array\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn last_basic() {
        let p = project_with(&[("main.yml", "arr: [1, 2, 3]\nmain: $last(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(3)
        );
    }

    #[test]
    fn last_empty_array() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $last(${arr()})\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn last_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $last(123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn sort_ints() {
        let p = project_with(&[("main.yml", "arr: [3, 1, 2]\nmain: $sort(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn sort_strings() {
        let p = project_with(&[(
            "main.yml",
            "arr: [\"c\", \"a\", \"b\"]\nmain: $sort(${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![
                Value::string("a"),
                Value::string("b"),
                Value::string("c")
            ])
        );
    }

    #[test]
    fn sort_mixed_types_is_e011() {
        let p = project_with(&[("main.yml", "arr: [1, \"a\"]\nmain: $sort(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn sort_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $sort(\"not array\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn sort_empty_array() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $sort(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![])
        );
    }

    #[test]
    fn reverse_basic() {
        let p = project_with(&[("main.yml", "arr: [1, 2, 3]\nmain: $reverse(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(3), Value::int(2), Value::int(1)])
        );
    }

    #[test]
    fn reverse_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $reverse(123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn unique_basic() {
        let p = project_with(&[(
            "main.yml",
            "arr: [1, 2, 1, 3, 2]\nmain: $unique(${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn unique_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $unique(\"not array\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn flatten_basic() {
        let p = project_with(&[(
            "main.yml",
            "arr: [[1, 2], [3, [4]]]\nmain: $flatten(${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![
                Value::int(1),
                Value::int(2),
                Value::int(3),
                Value::array(vec![Value::int(4)])
            ])
        );
    }

    #[test]
    fn flatten_one_level() {
        let p = project_with(&[("main.yml", "arr: [1, [2], 3]\nmain: $flatten(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn flatten_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $flatten(\"not array\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn slice_basic() {
        let p = project_with(&[(
            "main.yml",
            "arr: [0, 1, 2, 3, 4]\nmain: $slice(${arr()}, 1, 3)\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2)])
        );
    }

    #[test]
    fn slice_negative_end() {
        let p = project_with(&[(
            "main.yml",
            "arr: [0, 1, 2, 3, 4]\nmain: $slice(${arr()}, 2, -1)\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn slice_clamped() {
        let p = project_with(&[(
            "main.yml",
            "arr: [0, 1, 2]\nmain: $slice(${arr()}, 0, 99)\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(0), Value::int(1), Value::int(2)])
        );
    }

    #[test]
    fn slice_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $slice(123, 0, 1)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn filter_basic() {
        let p = project_with(&[(
            "main.yml",
            "identity: \"$0\"\narr: [1, false, 2, null, 3]\nmain: $filter($identity, ${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn filter_empty_array() {
        let p = project_with(&[(
            "main.yml",
            "is_true: \"true\"\narr: []\nmain: $filter($is_true, ${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![])
        );
    }

    #[test]
    fn filter_not_array_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "is_true: \"true\"\nmain: $filter($is_true, 5)\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn filter_array_item_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "is_true: \"true\"\narr: [[1, 2]]\nmain: $filter($is_true, ${arr()})\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn filter_unknown_callable_is_e002() {
        let p = project_with(&[("main.yml", "arr: [1]\nmain: $filter($nope, ${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E002);
    }

    // =====================================================
    // Object builtins
    // =====================================================

    #[test]
    fn keys_basic() {
        let p = project_with(&[("main.yml", "obj:\n  b: 2\n  a: 1\nmain: $keys(${obj()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::string("b"), Value::string("a")])
        );
    }

    #[test]
    fn keys_not_object_is_e011() {
        let p = project_with(&[("main.yml", "main: $keys(\"not object\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn values_basic() {
        let p = project_with(&[(
            "main.yml",
            "obj:\n  b: 2\n  a: 1\nmain: $values(${obj()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(2), Value::int(1)])
        );
    }

    #[test]
    fn values_not_object_is_e011() {
        let p = project_with(&[("main.yml", "main: $values(123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn entries_basic() {
        let p = project_with(&[(
            "main.yml",
            "obj:\n  b: 2\n  a: 1\nmain: $entries(${obj()})\n",
        )]);
        let result = compile_ok(&p, "main", &crate::ir::Args::None);
        let Value::Array(entries) = result else {
            panic!("expected array");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            Value::object(IndexMap::from([
                ("key".to_string(), Value::string("b")),
                ("value".to_string(), Value::int(2)),
            ]))
        );
        assert_eq!(
            entries[1],
            Value::object(IndexMap::from([
                ("key".to_string(), Value::string("a")),
                ("value".to_string(), Value::int(1)),
            ]))
        );
    }

    #[test]
    fn entries_not_object_is_e011() {
        let p = project_with(&[("main.yml", "main: $entries([])\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn from_entries_basic() {
        let p = project_with(&[(
            "main.yml",
            "arr: [{key: \"a\", value: 1}, {key: \"b\", value: 2}]\nmain: $from_entries(${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([
                ("a".to_string(), Value::int(1)),
                ("b".to_string(), Value::int(2)),
            ]))
        );
    }

    #[test]
    fn from_entries_empty() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $from_entries(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::new())
        );
    }

    #[test]
    fn from_entries_later_wins() {
        let p = project_with(&[(
            "main.yml",
            "arr: [{key: \"a\", value: 1}, {key: \"a\", value: 2}]\nmain: $from_entries(${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([("a".to_string(), Value::int(2))]))
        );
    }

    #[test]
    fn from_entries_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $from_entries(\"not an array\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn pick_basic() {
        let p = project_with(&[(
            "main.yml",
            "obj:\n  a: 1\n  b: 2\n  c: 3\nmain: $pick(${obj()}, [\"a\", \"c\"])\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([
                ("a".to_string(), Value::int(1)),
                ("c".to_string(), Value::int(3)),
            ]))
        );
    }

    #[test]
    fn pick_nonexistent_key() {
        let p = project_with(&[(
            "main.yml",
            "obj:\n  a: 1\nmain: $pick(${obj()}, [\"nonexistent\"])\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::new())
        );
    }

    #[test]
    fn pick_first_arg_not_object_is_e011() {
        let p = project_with(&[("main.yml", "main: $pick(123, [])\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn pick_second_arg_not_array_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "obj:\n  a: 1\nmain: $pick(${obj()}, \"not array\")\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn omit_basic() {
        let p = project_with(&[(
            "main.yml",
            "obj:\n  a: 1\n  b: 2\n  c: 3\nmain: $omit(${obj()}, [\"b\"])\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([
                ("a".to_string(), Value::int(1)),
                ("c".to_string(), Value::int(3)),
            ]))
        );
    }

    #[test]
    fn omit_first_arg_not_object_is_e011() {
        let p = project_with(&[("main.yml", "main: $omit(123, [])\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn omit_second_arg_not_array_is_e011() {
        let p = project_with(&[("main.yml", "obj:\n  a: 1\nmain: $omit(${obj()}, 123)\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    // =====================================================
    // Type builtins
    // =====================================================

    #[test]
    fn type_null() {
        let p = project_with(&[("main.yml", "main: $type(null)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("null")
        );
    }

    #[test]
    fn type_bool() {
        let p = project_with(&[("main.yml", "main: $type(true)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("bool")
        );
    }

    #[test]
    fn type_int() {
        let p = project_with(&[("main.yml", "main: $type(1)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("int")
        );
    }

    #[test]
    fn type_float() {
        let p = project_with(&[("main.yml", "main: $type(1.5)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("float")
        );
    }

    #[test]
    fn type_string() {
        let p = project_with(&[("main.yml", "main: $type(\"hello\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("string")
        );
    }

    #[test]
    fn type_array() {
        let p = project_with(&[("main.yml", "arr: [1, 2]\nmain: $type(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("array")
        );
    }

    #[test]
    fn type_object() {
        let p = project_with(&[("main.yml", "obj:\n  a: 1\nmain: $type(${obj()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("object")
        );
    }

    #[test]
    fn is_array_true() {
        let p = project_with(&[("main.yml", "arr: [1, 2]\nmain: $is_array(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_array_false() {
        let p = project_with(&[("main.yml", "obj:\n  a: 1\nmain: $is_array(${obj()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(false)
        );
    }

    #[test]
    fn is_object_true() {
        let p = project_with(&[("main.yml", "obj:\n  a: 1\nmain: $is_object(${obj()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_object_false() {
        let p = project_with(&[("main.yml", "arr: [1, 2]\nmain: $is_object(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(false)
        );
    }

    #[test]
    fn is_string_true() {
        let p = project_with(&[("main.yml", "main: $is_string(\"hello\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_string_false() {
        let p = project_with(&[("main.yml", "main: $is_string(42)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(false)
        );
    }

    #[test]
    fn is_number_true_int() {
        let p = project_with(&[("main.yml", "main: $is_number(42)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_number_true_float() {
        let p = project_with(&[("main.yml", "main: $is_number(3.14)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_number_false() {
        let p = project_with(&[("main.yml", "main: $is_number(\"42\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(false)
        );
    }

    #[test]
    fn is_null_true() {
        let p = project_with(&[("main.yml", "main: $is_null(null)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_null_false() {
        let p = project_with(&[("main.yml", "main: $is_null(false)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(false)
        );
    }

    #[test]
    fn to_string_int() {
        let p = project_with(&[("main.yml", "main: $to_string(123)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("123")
        );
    }

    #[test]
    fn to_string_float() {
        let p = project_with(&[("main.yml", "main: $to_string(2.5)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("2.5")
        );
    }

    #[test]
    fn to_string_null() {
        let p = project_with(&[("main.yml", "main: $to_string(null)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("null")
        );
    }

    #[test]
    fn to_string_bool() {
        let p = project_with(&[("main.yml", "main: $to_string(true)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("true")
        );
    }

    #[test]
    fn to_number_int_string() {
        let p = project_with(&[("main.yml", "main: $to_number(\"42\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(42)
        );
    }

    #[test]
    fn to_number_float_string() {
        let p = project_with(&[("main.yml", "main: $to_number(\"3.5\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(3.5)
        );
    }

    #[test]
    fn to_number_not_parseable() {
        let p = project_with(&[("main.yml", "main: $to_number(\"hello\")\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn to_number_pass_through_int() {
        let p = project_with(&[("main.yml", "main: $to_number(123)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(123)
        );
    }

    #[test]
    fn to_number_pass_through_float() {
        let p = project_with(&[("main.yml", "main: $to_number(2.5)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(2.5)
        );
    }

    #[test]
    fn coalesce_first_non_null() {
        let p = project_with(&[("main.yml", "main: $coalesce(null, null, 1, 2)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(1)
        );
    }

    #[test]
    fn coalesce_false_is_not_null() {
        let p = project_with(&[("main.yml", "main: $coalesce(null, false, 0)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(false)
        );
    }

    #[test]
    fn coalesce_all_null() {
        let p = project_with(&[("main.yml", "main: $coalesce(null, null)\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn coalesce_zero_is_not_null() {
        let p = project_with(&[("main.yml", "main: $coalesce(null, 0)\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(0)
        );
    }

    #[test]
    fn coalesce_empty_string_is_not_null() {
        let p = project_with(&[("main.yml", "main: $coalesce(null, \"\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("")
        );
    }

    // =====================================================
    // Math aggregates
    // =====================================================

    #[test]
    fn sum_basic() {
        let p = project_with(&[("main.yml", "arr: [1, 2, 3]\nmain: $sum(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(6)
        );
    }

    #[test]
    fn sum_empty() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $sum(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(0)
        );
    }

    #[test]
    fn sum_floats() {
        let p = project_with(&[("main.yml", "arr: [1.5, 2.5]\nmain: $sum(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(4.0)
        );
    }

    #[test]
    fn sum_mixed_int_float() {
        let p = project_with(&[("main.yml", "arr: [1, 2.5]\nmain: $sum(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(3.5)
        );
    }

    #[test]
    fn sum_non_numeric_is_e011() {
        let p = project_with(&[("main.yml", "arr: [\"a\", \"b\"]\nmain: $sum(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn sum_not_array_is_e011() {
        let p = project_with(&[("main.yml", "main: $sum(\"not array\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn avg_basic() {
        let p = project_with(&[("main.yml", "arr: [1, 2, 3]\nmain: $avg(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(2.0)
        );
    }

    #[test]
    fn avg_empty() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $avg(${arr()})\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn avg_non_numeric_is_e011() {
        let p = project_with(&[("main.yml", "arr: [\"a\"]\nmain: $avg(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn min_basic() {
        let p = project_with(&[("main.yml", "arr: [3, 1, 2]\nmain: $min(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(1)
        );
    }

    #[test]
    fn min_empty() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $min(${arr()})\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn min_non_numeric_is_e011() {
        let p = project_with(&[("main.yml", "arr: [\"a\"]\nmain: $min(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn max_basic() {
        let p = project_with(&[("main.yml", "arr: [3, 1, 2]\nmain: $max(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(3)
        );
    }

    #[test]
    fn max_empty() {
        let p = project_with(&[("main.yml", "arr: []\nmain: $max(${arr()})\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn max_non_numeric_is_e011() {
        let p = project_with(&[("main.yml", "arr: [true]\nmain: $max(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    // =====================================================
    // Conditional builtins
    // =====================================================

    #[test]
    fn if_true() {
        let p = project_with(&[("main.yml", "main: $if(true, \"yes\", \"no\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("yes")
        );
    }

    #[test]
    fn if_false() {
        let p = project_with(&[("main.yml", "main: $if(false, \"yes\", \"no\")\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("no")
        );
    }

    #[test]
    fn if_null_condition_is_e011() {
        let p = project_with(&[("main.yml", "main: $if(null, \"yes\", \"no\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
        assert!(err.message.contains("Bool"), "{}", err.message);
    }

    #[test]
    fn if_zero_condition_is_e011() {
        let p = project_with(&[("main.yml", "main: $if(0, \"yes\", \"no\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn if_wrong_arg_count_is_e011() {
        let p = project_with(&[("main.yml", "main: $if(true, \"yes\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn if_lazy_then_branch() {
        let p = project_with(&[(
            "main.yml",
            "bad: $noexist()\nmain: $if(true, \"ok\", ${bad()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("ok")
        );
    }

    #[test]
    fn if_lazy_else_branch() {
        let p = project_with(&[(
            "main.yml",
            "bad: $noexist()\nmain: $if(false, ${bad()}, \"ok\")\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("ok")
        );
    }

    #[test]
    fn when_basic() {
        let p = project_with(&[(
            "main.yml",
            "double: \"${$0 * 2}\"\narr: [1, 2, 3]\nmain: $when($double, ${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(2), Value::int(4), Value::int(6)])
        );
    }

    #[test]
    fn when_filters_falsy() {
        let p = project_with(&[(
            "main.yml",
            "identity: \"$0\"\narr: [1, false, 2, null, 3]\nmain: $when($identity, ${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn when_empty_array() {
        let p = project_with(&[(
            "main.yml",
            "double: \"${$0 * 2}\"\narr: []\nmain: $when($double, ${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![])
        );
    }

    #[test]
    fn when_not_array_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "double: \"${$0 * 2}\"\nmain: $when($double, 5)\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn when_array_item_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "double: \"${$0 * 2}\"\narr: [[1, 2]]\nmain: $when($double, ${arr()})\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn when_unknown_callable_is_e002() {
        let p = project_with(&[("main.yml", "arr: [1]\nmain: $when($nope, ${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E002);
    }

    // =====================================================
    // Component reference args
    // =====================================================

    #[test]
    fn split_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "my_str: \"a,b,c\"\ndelim: \",\"\nmain: $split(${my_str()}, ${delim()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![
                Value::string("a"),
                Value::string("b"),
                Value::string("c")
            ])
        );
    }

    #[test]
    fn join_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "my_arr: [\"a\", \"b\"]\nsep: \"-\"\nmain: $join(${my_arr()}, ${sep()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("a-b")
        );
    }

    #[test]
    fn trim_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_str: \"  hello  \"\nmain: $trim(${my_str()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello")
        );
    }

    #[test]
    fn upper_with_component_ref() {
        let p = project_with(&[("main.yml", "my_str: \"hello\"\nmain: $upper(${my_str()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("HELLO")
        );
    }

    #[test]
    fn lower_with_component_ref() {
        let p = project_with(&[("main.yml", "my_str: \"HELLO\"\nmain: $lower(${my_str()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello")
        );
    }

    #[test]
    fn replace_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "my_str: \"hello world\"\nold: \"world\"\nnew: \"rust\"\nmain: $replace(${my_str()}, ${old()}, ${new()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("hello rust")
        );
    }

    #[test]
    fn first_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [1, 2, 3]\nmain: $first(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(1)
        );
    }

    #[test]
    fn last_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [1, 2, 3]\nmain: $last(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(3)
        );
    }

    #[test]
    fn sort_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [3, 1, 2]\nmain: $sort(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn reverse_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_arr: [1, 2, 3]\nmain: $reverse(${my_arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(3), Value::int(2), Value::int(1)])
        );
    }

    #[test]
    fn unique_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_arr: [1, 2, 1, 3]\nmain: $unique(${my_arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn flatten_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_arr: [[1, 2], [3]]\nmain: $flatten(${my_arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn slice_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_arr: [0, 1, 2, 3, 4]\nmain: $slice(${my_arr()}, 1, 3)\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2)])
        );
    }

    #[test]
    fn filter_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "identity: \"$0\"\nnums: [1, false, 2, null, 3]\nmain: $filter($identity, ${nums()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn keys_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_obj:\n  b: 2\n  a: 1\nmain: $keys(${my_obj()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::string("b"), Value::string("a")])
        );
    }

    #[test]
    fn values_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_obj:\n  b: 2\n  a: 1\nmain: $values(${my_obj()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(2), Value::int(1)])
        );
    }

    #[test]
    fn entries_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_obj:\n  b: 2\n  a: 1\nmain: $entries(${my_obj()})\n",
        )]);
        let result = compile_ok(&p, "main", &crate::ir::Args::None);
        let Value::Array(entries) = result else {
            panic!("expected array");
        };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn from_entries_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_arr: [{key: \"a\", value: 1}]\nmain: $from_entries(${my_arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([("a".to_string(), Value::int(1))]))
        );
    }

    #[test]
    fn pick_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "my_obj:\n  a: 1\n  b: 2\n  c: 3\nkey_list: [\"a\", \"c\"]\nmain: $pick(${my_obj()}, ${key_list()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([
                ("a".to_string(), Value::int(1)),
                ("c".to_string(), Value::int(3)),
            ]))
        );
    }

    #[test]
    fn omit_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "my_obj:\n  a: 1\n  b: 2\n  c: 3\nkey_list: [\"b\"]\nmain: $omit(${my_obj()}, ${key_list()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::object(IndexMap::from([
                ("a".to_string(), Value::int(1)),
                ("c".to_string(), Value::int(3)),
            ]))
        );
    }

    #[test]
    fn type_with_component_ref() {
        let p = project_with(&[("main.yml", "my_val: \"hello\"\nmain: $type(${my_val()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("string")
        );
    }

    #[test]
    fn is_array_with_component_ref() {
        let p = project_with(&[("main.yml", "my_val: [1, 2]\nmain: $is_array(${my_val()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_object_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_val:\n  a: 1\nmain: $is_object(${my_val()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_string_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_val: \"hello\"\nmain: $is_string(${my_val()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_number_with_component_ref() {
        let p = project_with(&[("main.yml", "my_val: 42\nmain: $is_number(${my_val()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn is_null_with_component_ref() {
        let p = project_with(&[("main.yml", "my_val: null\nmain: $is_null(${my_val()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::bool(true)
        );
    }

    #[test]
    fn to_string_with_component_ref() {
        let p = project_with(&[("main.yml", "my_val: 123\nmain: $to_string(${my_val()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("123")
        );
    }

    #[test]
    fn to_number_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "my_str: \"3.5\"\nmain: $to_number(${my_str()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(3.5)
        );
    }

    #[test]
    fn coalesce_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "a: null\nb: null\nmain: $coalesce(${a()}, ${b()}, 1)\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(1)
        );
    }

    #[test]
    fn sum_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [1, 2, 3]\nmain: $sum(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(6)
        );
    }

    #[test]
    fn avg_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [1, 2, 3]\nmain: $avg(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(2.0)
        );
    }

    #[test]
    fn min_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [3, 1, 2]\nmain: $min(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(1)
        );
    }

    #[test]
    fn max_with_component_ref() {
        let p = project_with(&[("main.yml", "my_arr: [3, 1, 2]\nmain: $max(${my_arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(3)
        );
    }

    #[test]
    fn if_with_component_refs() {
        let p = project_with(&[(
            "main.yml",
            "cond: true\nyes: \"yes\"\nno: \"no\"\nmain: $if(${cond()}, ${yes()}, ${no()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::string("yes")
        );
    }

    #[test]
    fn when_with_component_ref() {
        let p = project_with(&[(
            "main.yml",
            "double: \"${$0 * 2}\"\nnums: [1, 2, 3]\nmain: $when($double, ${nums()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(2), Value::int(4), Value::int(6)])
        );
    }

    // =====================================================
    // Additional E011 cases
    // =====================================================

    #[test]
    fn sum_with_boolean_is_e011() {
        let p = project_with(&[("main.yml", "arr: [true, false]\nmain: $sum(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn avg_with_string_is_e011() {
        let p = project_with(&[("main.yml", "arr: [\"a\"]\nmain: $avg(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn min_with_string_is_e011() {
        let p = project_with(&[("main.yml", "arr: [\"a\"]\nmain: $min(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn max_with_boolean_is_e011() {
        let p = project_with(&[("main.yml", "arr: [true]\nmain: $max(${arr()})\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn slice_second_arg_not_int_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "arr: [0, 1, 2]\nmain: $slice(${arr()}, \"a\", 2)\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn slice_third_arg_not_int_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "arr: [0, 1, 2]\nmain: $slice(${arr()}, 0, \"b\")\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn from_entries_element_not_object_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "arr: [\"not an object\"]\nmain: $from_entries(${arr()})\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn from_entries_missing_key_field_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "arr: [{wrong: \"field\"}]\nmain: $from_entries(${arr()})\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn from_entries_key_not_string_is_e011() {
        let p = project_with(&[(
            "main.yml",
            "arr: [{key: 1, value: 2}]\nmain: $from_entries(${arr()})\n",
        )]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn pick_key_list_non_string_is_e011() {
        let p = project_with(&[("main.yml", "obj:\n  a: 1\nmain: $pick(${obj()}, [123])\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn omit_key_list_non_string_is_e011() {
        let p = project_with(&[("main.yml", "obj:\n  a: 1\nmain: $omit(${obj()}, [123])\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn sort_floats() {
        let p = project_with(&[("main.yml", "arr: [3.0, 1.0, 2.0]\nmain: $sort(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![
                Value::float(1.0),
                Value::float(2.0),
                Value::float(3.0)
            ])
        );
    }

    #[test]
    fn join_object_elements_is_e011() {
        let p = project_with(&[("main.yml", "arr: [{a: 1}]\nmain: $join(${arr()}, \",\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn join_array_elements_is_e011() {
        let p = project_with(&[("main.yml", "arr: [[1, 2]]\nmain: $join(${arr()}, \",\")\n")]);
        let err = compile_err(&p, "main", &crate::ir::Args::None);
        assert_eq!(err.code, E011);
    }

    #[test]
    fn to_number_bool_is_null() {
        let p = project_with(&[("main.yml", "main: $to_number(true)\n")]);
        assert_eq!(compile_ok(&p, "main", &crate::ir::Args::None), Value::Null);
    }

    #[test]
    fn when_transform_and_filter_combined() {
        let p = project_with(&[(
            "main.yml",
            "add_ten: \"${$0 + 10}\"\narr: [5, 0, -3]\nmain: $when($add_ten, ${arr()})\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::array(vec![Value::int(15), Value::int(10), Value::int(7)])
        );
    }

    #[test]
    fn reduce_with_init_value() {
        let p = project_with(&[(
            "main.yml",
            "add: \"${$0 + last}\"\narr: [1, 2, 3]\nmain: $reduce($add, ${arr()}, 10)\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::int(16)
        );
    }

    #[test]
    fn sum_float_array() {
        let p = project_with(&[("main.yml", "arr: [1.5, 2.5, 3.0]\nmain: $sum(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(7.0)
        );
    }

    #[test]
    fn min_float() {
        let p = project_with(&[("main.yml", "arr: [3.14, 1.5, 2.7]\nmain: $min(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(1.5)
        );
    }

    #[test]
    fn max_float() {
        let p = project_with(&[("main.yml", "arr: [3.5, 1.5, 2.7]\nmain: $max(${arr()})\n")]);
        assert_eq!(
            compile_ok(&p, "main", &crate::ir::Args::None),
            Value::float(3.5)
        );
    }
}
