//! String interpolation: scanning YAML string scalars into segments and
//! resolving them per the PRD *String syntax* rules.
//!
//! [`scan`] parses a string scalar into [`Segment`]s: literal text, `$name` /
//! `$N` argument references, and `${...}` math expressions, resolving the
//! `\$` / `\\` escapes (any other `\X` is `E010`, as is a dangling `$` or an
//! unterminated `${...}`). [`resolve`] turns the segments into a [`Value`]: a
//! single interpolation keeps the interpoland's native type (Int, Float, Bool,
//! String, Null, Object, Array — whatever the argument or the math engine
//! yields); with surrounding text the result is a String in which each
//! interpoland is rendered via the shared [`render_value`] helper (objects and
//! arrays into text are `E011`).

use crate::callsite;
use crate::diag::{Diagnostic, Span, E003, E010, E011};
use crate::ir::{render_value, NoStringRender, Value};
use crate::math::{MathEngine, Scope};

/// One piece of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// Literal text; `\$` / `\\` escapes already resolved.
    Text(String),
    /// A `$name` or `$N` reference (`name` holds the digits for `$N`).
    Arg { name: String, span: Span },
    /// A `${...}` math expression; `src` is the raw text between the braces.
    Math { src: String, span: Span },
    /// A `$name(args)` component call inside shell interpolation.
    Call {
        name: String,
        args: String,
        span: Span,
    },
    /// A `$name{...}` brace call (rule 22): `name` is the effective identifier,
    /// `payload_src` is the raw text between the braces (balanced, quote-aware).
    BraceCall {
        name: String,
        payload_src: String,
        span: Span,
    },
}

/// Scan a YAML string scalar into interpolation [`Segment`]s.
///
/// Grammar (PRD *String syntax*): `$name` where `name` is
/// `[A-Za-z_][A-Za-z0-9_]*`; `$0`/`$1`/… positional references (pure decimal
/// digits); `${...}` math context (closed by the first `}` — the math grammar
/// has no `}`); escapes `\$` and `\\` producing a literal `$` / `\`. Any other
/// `\X` is `E010`; a `$` not followed by a name, digits, or `{` is `E010`; an
/// unterminated `${...}` is `E010`.
///
/// `base` is the span of the scalar's first character; segment spans are
/// computed relative to it (advancing line/col across newlines).
pub fn scan(src: &str, base: Span) -> Result<Vec<Segment>, Diagnostic> {
    scan_impl(src, base, false)
}

/// Scan a shell command string into interpolation [`Segment`]s.
pub fn scan_shell(src: &str, base: Span) -> Result<Vec<Segment>, Diagnostic> {
    scan_impl(src, base, true)
}

fn scan_impl(src: &str, base: Span, shell_calls: bool) -> Result<Vec<Segment>, Diagnostic> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut text = String::new();
    let mut chars = src.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        match c {
            '\\' => {
                let esc_span = span_at(base, src, idx);
                match chars.next() {
                    Some((_, '$')) => text.push('$'),
                    Some((_, '\\')) => text.push('\\'),
                    Some((_, other)) => {
                        return Err(e010(
                            esc_span,
                            format!(
                                "invalid escape `\\{other}` in string (only `\\$` and `\\\\` are allowed)"
                            ),
                        ));
                    }
                    None => {
                        return Err(e010(
                            esc_span,
                            "trailing escape `\\` in string (only `\\$` and `\\\\` are allowed)"
                                .to_string(),
                        ));
                    }
                }
            }
            '$' => {
                if !text.is_empty() {
                    segments.push(Segment::Text(std::mem::take(&mut text)));
                }
                match chars.peek() {
                    Some((_, c)) if *c == '{' => {
                        chars.next();
                        let mut math_src = String::new();
                        let mut closed = false;
                        for (_, mc) in chars.by_ref() {
                            if mc == '}' {
                                closed = true;
                                break;
                            }
                            math_src.push(mc);
                        }
                        if !closed {
                            return Err(e010(
                                span_at(base, src, idx),
                                "unterminated `${...}` in string".to_string(),
                            ));
                        }
                        segments.push(Segment::Math {
                            src: math_src,
                            span: span_at(base, src, idx),
                        });
                    }
                    Some((_, c)) if c.is_ascii_alphabetic() || *c == '_' => {
                        let start = idx;
                        let mut name = String::new();
                        while let Some((_, nc)) = chars.peek() {
                            if nc.is_ascii_alphanumeric() || *nc == '_' {
                                name.push(*nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        if chars.peek().map(|(_, c)| *c) == Some('(') {
                            let rest = &src[start..];
                            match callsite::parse_prefix(rest) {
                                Ok(Some((call, consumed))) => {
                                    let is_shell_call = shell_calls;
                                    let has_named_args = call.args.iter().any(|a| a.key.is_some());
                                    if is_shell_call || has_named_args {
                                        let span = span_at(base, src, start);
                                        debug_assert_eq!(call.name, name);
                                        let args_start = call.name.len() + 2;
                                        let args_end = consumed.saturating_sub(1);
                                        let args = rest[args_start..args_end].to_string();
                                        let target = start + consumed;
                                        while let Some(&(next_idx, _)) = chars.peek() {
                                            if next_idx < target {
                                                chars.next();
                                            } else {
                                                break;
                                            }
                                        }
                                        segments.push(Segment::Call {
                                            name: call.name,
                                            args,
                                            span,
                                        });
                                        continue;
                                    }
                                }
                                Ok(None) => {}
                                Err((code, message)) => {
                                    return Err(Diagnostic {
                                        file: None,
                                        line: span_at(base, src, start).line,
                                        col: span_at(base, src, start).col,
                                        component: None,
                                        code,
                                        message,
                                    });
                                }
                            }
                        }
                        // Check for `$name{...}` brace call (rule 22).
                        if chars.peek().map(|(_, c)| *c) == Some('{') {
                            chars.next(); // consume '{'
                            let span_start = start;
                            // Compute the remaining string after the opening '{' using byte indices
                            let after_open = start + name.len() + 2; // start + len(name) + 1('$') + 1('{')
                            let rest = &src[after_open..];
                            match find_matching_brace(rest) {
                                Some((payload, _)) => {
                                    // The closing brace is at after_open + payload.len() + 1 (the '}')
                                    // Skip chars iterator to the position after the closing '}'
                                    let closing_pos = after_open + payload.len() + 1;
                                    while let Some((next_idx, _)) = chars.peek() {
                                        if *next_idx < closing_pos {
                                            chars.next();
                                        } else {
                                            break;
                                        }
                                    }
                                    segments.push(Segment::BraceCall {
                                        name,
                                        payload_src: payload.to_string(),
                                        span: span_at(base, src, span_start),
                                    });
                                }
                                None => {
                                    return Err(e010(
                                        span_at(base, src, start),
                                        format!(
                                            "unterminated `${{{name}{{...}}}}` brace call in string"
                                        ),
                                    ));
                                }
                            }
                        } else {
                            segments.push(Segment::Arg {
                                name,
                                span: span_at(base, src, start),
                            });
                        }
                    }
                    Some((_, c)) if c.is_ascii_digit() => {
                        let start = idx;
                        let mut name = String::new();
                        while let Some((_, nc)) = chars.peek() {
                            if nc.is_ascii_digit() {
                                name.push(*nc);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        segments.push(Segment::Arg {
                            name,
                            span: span_at(base, src, start),
                        });
                    }
                    _ => {
                        return Err(e010(
                            span_at(base, src, idx),
                            "dangling `$` in string (expected `$name`, `$N`, or `${...}`)"
                                .to_string(),
                        ));
                    }
                }
            }
            _ => text.push(c),
        }
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    Ok(segments)
}

/// Resolve scanned [`Segment`]s against `scope` into a [`Value`].
///
/// A single interpolation (`$name`, `$N`, or `${...}` with no surrounding
/// text) yields the interpoland's native type. Any mixture of text and
/// interpolations yields a String in which each interpoland is rendered via
/// the shared [`render_value`] helper (PRD *Number→string rendering*: Int
/// plain, Float via [`render_f64`](crate::ir::render_f64), Bool
/// `true`/`false`, Null `null`); Objects and Arrays rendered into text are
/// `E011`.
///
/// `${...}` segments are evaluated through `engine` (the [`MathEngine`]
/// boundary); `$name` / `$N` segments resolve against the scope's named /
/// positional arguments (and the reduce-step `last` via
/// [`Scope::lookup`]); a missing argument is `E003`.
pub fn resolve(
    segments: &[Segment],
    scope: &Scope<'_>,
    engine: &dyn MathEngine,
) -> Result<Value, Diagnostic> {
    match segments {
        [Segment::Text(t)] => Ok(Value::string(t.clone())),
        [Segment::Arg { name, span }] => resolve_arg(name, *span, scope),
        [Segment::Math { src, span }] => {
            // Check if the math source is a shell-style call with named arguments
            // (e.g., `b(x=12, y=34)`). These cannot be parsed by the math engine
            // because `=` is not a math token. Route them through resolve_shell_call.
            // Component calls with positional or no args can be handled by engine.eval.
            let with_dollar = format!("${src}");
            if let Ok(Some((call, consumed))) = callsite::parse_prefix(&with_dollar) {
                if consumed == with_dollar.len() && call.args.iter().any(|a| a.key.is_some()) {
                    // Named arguments present - route through shell_call hook.
                    let v = scope.invoke_shell(&call, *span)?;
                    // If the result is a string, re-evaluate it as math
                    if let Value::String(s) = &v {
                        return engine.eval(s, scope);
                    }
                    return render_into_text(&v, scope, *span).map(Value::string);
                }
            }
            engine
                .eval(src, scope)
                .or_else(|_| resolve_named_arg_math(src, scope, *span, engine))
        }
        [Segment::Call { name, args, span }] => {
            Ok(Value::string(resolve_shell_call(name, args, *span, scope)?))
        }
        [Segment::BraceCall {
            name,
            payload_src,
            span,
        }] => {
            let payload =
                callsite::parse_brace_payload(payload_src).map_err(|(code, msg)| Diagnostic {
                    file: scope.file.clone(),
                    line: span.line,
                    col: span.col,
                    component: scope.component.clone(),
                    code,
                    message: msg,
                })?;
            scope.invoke_brace_call(name, &payload, *span)
        }
        _ => {
            let mut out = String::new();
            for seg in segments {
                match seg {
                    Segment::Text(t) => out.push_str(t),
                    Segment::Arg { name, span } => {
                        let v = resolve_arg(name, *span, scope)?;
                        out.push_str(&render_into_text(&v, scope, *span)?);
                    }
                    Segment::Math { src, span } => {
                        let v = match engine.eval(src, scope) {
                            Ok(v) => v,
                            Err(_) => resolve_named_arg_math(src, scope, *span, engine)?,
                        };
                        out.push_str(&render_into_text(&v, scope, *span)?);
                    }
                    Segment::Call { name, args, span } => {
                        out.push_str(&resolve_shell_call(name, args, *span, scope)?);
                    }
                    Segment::BraceCall {
                        name,
                        payload_src,
                        span,
                    } => {
                        let payload =
                            callsite::parse_brace_payload(payload_src).map_err(|(code, msg)| {
                                Diagnostic {
                                    file: scope.file.clone(),
                                    line: span.line,
                                    col: span.col,
                                    component: scope.component.clone(),
                                    code,
                                    message: msg,
                                }
                            })?;
                        let v = scope.invoke_brace_call(name, &payload, *span)?;
                        out.push_str(&render_into_text(&v, scope, *span)?);
                    }
                }
            }
            Ok(Value::string(out))
        }
    }
}

/// Resolve shell-command interpolation segments into a String.
///
/// Shell command strings always render interpolations to text. In addition to
/// the regular `$name` / `$0` / `${...}` forms, shell interpolation accepts
/// `$name(args)` component calls.
pub fn resolve_shell(
    segments: &[Segment],
    scope: &Scope<'_>,
    engine: &dyn MathEngine,
) -> Result<Value, Diagnostic> {
    let mut out = String::new();
    for seg in segments {
        match seg {
            Segment::Text(t) => out.push_str(t),
            Segment::Arg { name, span } => {
                let v = resolve_arg(name, *span, scope)?;
                out.push_str(&render_into_text(&v, scope, *span)?);
            }
            Segment::Math { src, span } => {
                // Check if the math source is a shell-style call with named arguments.
                let with_dollar = format!("${src}");
                if let Ok(Some((call, consumed))) = callsite::parse_prefix(&with_dollar) {
                    if consumed == with_dollar.len() && call.args.iter().any(|a| a.key.is_some()) {
                        // Named arguments present - route through shell_call hook.
                        let v = scope.invoke_shell(&call, *span)?;
                        out.push_str(&render_into_text(&v, scope, *span)?);
                        continue;
                    }
                }
                let v = engine.eval(src, scope)?;
                out.push_str(&render_into_text(&v, scope, *span)?);
            }
            Segment::Call { name, args, span } => {
                out.push_str(&resolve_shell_call(name, args, *span, scope)?);
            }
            Segment::BraceCall {
                name,
                payload_src,
                span,
            } => {
                let payload =
                    callsite::parse_brace_payload(payload_src).map_err(|(code, msg)| {
                        Diagnostic {
                            file: scope.file.clone(),
                            line: span.line,
                            col: span.col,
                            component: scope.component.clone(),
                            code,
                            message: msg,
                        }
                    })?;
                let v = scope.invoke_brace_call(name, &payload, *span)?;
                out.push_str(&render_into_text(&v, scope, *span)?);
            }
        }
    }
    Ok(Value::string(out))
}

/// Resolve a shell call segment (`$name(args)`) to rendered text.
fn resolve_shell_call(
    name: &str,
    args: &str,
    span: Span,
    scope: &Scope<'_>,
) -> Result<String, Diagnostic> {
    let call_src = format!("${name}({args})");
    let call = match callsite::parse(&call_src) {
        Ok(Some(call)) => call,
        Ok(None) => unreachable!("constructed shell call did not parse"),
        Err((code, message)) => return Err(ctx_err(scope, span, code, message)),
    };
    let v = scope.invoke_shell(&call, span)?;
    render_into_text(&v, scope, span)
}

/// Resolve a `$name` / `$N` argument reference.
///
/// `$N` resolves positionally (rule 4); a missing positional is `E003`. A
/// named `$name` resolves in rule-2 order: (a) the named argument in scope;
/// (b) else `E003`.
fn resolve_arg(name: &str, span: Span, scope: &Scope<'_>) -> Result<Value, Diagnostic> {
    if name.bytes().all(|b| b.is_ascii_digit()) {
        return match name.parse::<usize>() {
            Ok(index) => match scope.positional_at(index) {
                Some(v) => Ok(v.clone()),
                None => Err(ctx_err(
                    scope,
                    span,
                    E003,
                    format!("missing required argument `${name}`"),
                )),
            },
            Err(_) => Err(ctx_err(
                scope,
                span,
                E003,
                format!("missing required argument `${name}`"),
            )),
        };
    }
    match scope.lookup(name) {
        Some(v) => Ok(v.clone()),
        None => Err(ctx_err(
            scope,
            span,
            E003,
            format!("missing required argument `{name}`"),
        )),
    }
}

/// Render an interpolated value into surrounding text via the shared
/// [`render_value`] helper; Objects and Arrays are `E011`.
fn render_into_text(v: &Value, scope: &Scope<'_>, span: Span) -> Result<String, Diagnostic> {
    match render_value(v) {
        Ok(s) => Ok(s),
        Err(NoStringRender) => Err(ctx_err(
            scope,
            span,
            E011,
            "objects and arrays have no string rendering".to_string(),
        )),
    }
}

/// Span (line/col) at byte `offset` inside `src`, relative to `base`.
fn span_at(base: Span, src: &str, offset: usize) -> Span {
    let (line, col) = src[..offset]
        .chars()
        .fold((base.line, base.col), |(l, c), ch| {
            if ch == '\n' {
                (l + 1, 1)
            } else {
                (l, c + 1)
            }
        });
    Span { line, col }
}

/// Try to evaluate a math expression that may contain named-arg calls
/// (e.g. `b(x=12, y=34) + b(x=12, y=34)`). The math engine can't parse
/// `=` tokens, so we pre-evaluate each named-arg call via invoke_shell
/// and substitute the text results back into the expression.
fn resolve_named_arg_math(
    src: &str,
    scope: &Scope<'_>,
    span: Span,
    engine: &dyn MathEngine,
) -> Result<Value, Diagnostic> {
    let mut result = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let is_dollar = bytes[i] == b'$';
        let name_start = if is_dollar { i + 1 } else { i };

        if name_start < bytes.len()
            && (bytes[name_start].is_ascii_alphabetic() || bytes[name_start] == b'_')
        {
            let mut j = name_start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }

            if j < bytes.len() && bytes[j] == b'(' {
                let call_src = &src[i..];
                let with_dollar = if is_dollar {
                    call_src.to_string()
                } else {
                    format!("${call_src}")
                };

                if let Ok(Some((call, consumed))) = callsite::parse_prefix(&with_dollar) {
                    if call.args.iter().any(|a| a.key.is_some()) {
                        let v = scope.invoke_shell(&call, span)?;
                        let text = render_into_text(&v, scope, span)?;
                        result.push_str(&text);
                        i += if is_dollar { consumed } else { consumed - 1 };
                        continue;
                    }
                }
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    engine.eval(&result, scope)
}

/// Find the closing `}` for an opening `{` at the start of `s`,
/// tracking nested `{}` and respecting `'"/"` quotes (with backslash escapes).
/// Returns `Some((payload, remaining))` where `payload` is the text between the
/// braces (balanced, quote-aware) and `remaining` is the text after the closing `}`.
/// `None` if unterminated.
fn find_matching_brace(s: &str) -> Option<(String, String)> {
    let mut depth = 1usize;
    let mut payload = String::new();
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                payload.push(c);
                if let Some(nc) = chars.next() {
                    payload.push(nc);
                }
                continue;
            }
            if c == q {
                quote = None;
            }
            payload.push(c);
        } else {
            match c {
                '\'' | '"' => {
                    quote = Some(c);
                    payload.push(c);
                }
                '{' => {
                    depth += 1;
                    payload.push(c);
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let remaining: String = chars.collect();
                        return Some((payload, remaining));
                    }
                    payload.push(c);
                }
                _ => {
                    payload.push(c);
                }
            }
        }
    }
    None
}

/// A syntax-error diagnostic (`E010`) for a scan failure.
fn e010(span: Span, message: String) -> Diagnostic {
    Diagnostic {
        file: None,
        line: span.line,
        col: span.col,
        component: None,
        code: E010,
        message,
    }
}

/// A diagnostic attributed to `scope`'s file/component context at `span`.
fn ctx_err(scope: &Scope, span: Span, code: &'static str, message: String) -> Diagnostic {
    Diagnostic {
        file: scope.file.clone(),
        line: span.line,
        col: span.col,
        component: scope.component.clone(),
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use std::rc::Rc;

    const SPAN: Span = Span { line: 1, col: 1 };

    /// Fake engine for task-1 tests (the real v1 evaluator lands with the
    /// math tasks): `${n}` evaluates to the Int literal `n`, else `0`.
    struct FakeEngine;

    impl MathEngine for FakeEngine {
        fn eval(&self, src: &str, _scope: &Scope<'_>) -> Result<Value, Diagnostic> {
            Ok(Value::int(src.trim().parse().unwrap_or(0)))
        }
    }

    fn named(entries: &[(&str, Value)]) -> Vec<(String, Value)> {
        entries
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect()
    }

    fn scope_of<'a>(entries: &[(&'a str, Value)]) -> Scope<'a> {
        Scope::with_args(named(entries), vec![])
    }

    fn obj() -> Value {
        Value::object(IndexMap::from([("k".to_string(), Value::int(1))]))
    }

    #[test]
    fn single_interpolation_keeps_native_type() {
        let scope = scope_of(&[("user_phone", Value::int(123456789))]);
        let segs = scan("$user_phone", SPAN).unwrap();
        assert_eq!(
            resolve(&segs, &scope, &FakeEngine).unwrap(),
            Value::int(123456789)
        );
    }

    #[test]
    fn single_interpolation_keeps_float_bool_string_null() {
        let scope = scope_of(&[
            ("ratio", Value::float(2.0)),
            ("flag", Value::bool(true)),
            ("text", Value::string("x")),
            ("nothing", Value::null()),
        ]);
        assert_eq!(
            resolve(&scan("$ratio", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::float(2.0)
        );
        assert_eq!(
            resolve(&scan("$flag", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::bool(true)
        );
        assert_eq!(
            resolve(&scan("$text", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("x")
        );
        assert_eq!(
            resolve(&scan("$nothing", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::null()
        );
    }

    #[test]
    fn single_interpolation_keeps_object_and_array() {
        let scope = scope_of(&[("obj", obj())]);
        assert_eq!(
            resolve(&scan("$obj", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            obj()
        );
        let arr = Value::array(vec![Value::int(1), Value::int(2)]);
        let scope = scope_of(&[("arr", arr.clone())]);
        assert_eq!(
            resolve(&scan("$arr", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            arr
        );
    }

    #[test]
    fn single_positional_interpolation_keeps_native_type() {
        let scope = Scope::with_args(vec![], vec![Value::int(46)]);
        assert_eq!(
            resolve(&scan("$0", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::int(46)
        );
    }

    #[test]
    fn surrounding_text_concatenates_rendered_values() {
        let scope = scope_of(&[
            ("name", Value::string("Mathew")),
            ("phone", Value::int(123456789)),
            ("ratio", Value::float(2.5)),
            ("whole", Value::float(2.0)),
            ("flag", Value::bool(true)),
            ("nothing", Value::null()),
        ]);
        assert_eq!(
            resolve(&scan("hi $name", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("hi Mathew")
        );
        assert_eq!(
            resolve(&scan("$name!", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("Mathew!")
        );
        assert_eq!(
            resolve(
                &scan("p=$phone r=$ratio w=$whole f=$flag n=$nothing", SPAN).unwrap(),
                &scope,
                &FakeEngine
            )
            .unwrap(),
            Value::string("p=123456789 r=2.5 w=2.0 f=true n=null")
        );
    }

    #[test]
    fn float_inside_text_keeps_fractional_part() {
        // Integer-valued floats render with their fractional part inside
        // interpolation (shared renderer; never Rust's `{}`).
        let scope = scope_of(&[("ratio", Value::float(2.0))]);
        assert_eq!(
            resolve(&scan("ratio=$ratio", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("ratio=2.0")
        );
        let scope = scope_of(&[("half", Value::float(2.5))]);
        assert_eq!(
            resolve(&scan("half=$half", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("half=2.5")
        );
    }

    #[test]
    fn mixed_named_positional_and_text() {
        let scope = Scope::with_args(
            named(&[("b", Value::int(6))]),
            vec![Value::int(12), Value::int(34)],
        );
        assert_eq!(
            resolve(&scan("$0 + $1 = $b", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("12 + 34 = 6")
        );
    }

    #[test]
    fn escapes_produce_literal_characters() {
        let scope = scope_of(&[]);
        assert_eq!(
            resolve(
                &scan(r"cost: \$5 and \\", SPAN).unwrap(),
                &scope,
                &FakeEngine
            )
            .unwrap(),
            Value::string(r"cost: $5 and \")
        );
        assert_eq!(
            scan(r"\$", SPAN).unwrap(),
            vec![Segment::Text("$".to_string())]
        );
        assert_eq!(
            scan(r"\\", SPAN).unwrap(),
            vec![Segment::Text("\\".to_string())]
        );
    }

    #[test]
    fn invalid_escape_is_e010() {
        for bad in [r"\q", r"a\1", r"\ ", r"\n", r"abc\"] {
            let err = scan(bad, SPAN).unwrap_err();
            assert_eq!(err.code, E010, "{bad}");
            assert!(err.message.contains("escape"), "{bad}: {}", err.message);
        }
    }

    #[test]
    fn dangling_dollar_is_e010() {
        for bad in ["abc $", "a $ b", "$.", "$$x", "$-", "$:"] {
            let err = scan(bad, SPAN).unwrap_err();
            assert_eq!(err.code, E010, "{bad}");
        }
    }

    #[test]
    fn unterminated_math_brace_is_e010() {
        let err = scan("${1 + 2", SPAN).unwrap_err();
        assert_eq!(err.code, E010);
        assert!(err.message.contains("${"), "{}", err.message);
    }

    #[test]
    fn object_or_array_into_text_is_e011() {
        let scope = scope_of(&[("obj", obj())]);
        let err = resolve(&scan("v=$obj!", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E011);

        let arr = Value::array(vec![Value::int(1)]);
        let scope = scope_of(&[("arr", arr)]);
        let err = resolve(&scan("[$arr]", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E011);

        use crate::math::V1Engine;
        let scope = scope_of(&[("obj", obj())]);
        let err = resolve(&scan("v=${ obj }", SPAN).unwrap(), &scope, &V1Engine).unwrap_err();
        assert_eq!(err.code, E011, "math Object into surrounding text");

        let scope = scope_of(&[("arr", Value::array(vec![Value::int(1)]))]);
        let err = resolve(&scan("v=${ arr }", SPAN).unwrap(), &scope, &V1Engine).unwrap_err();
        assert_eq!(err.code, E011, "math Array into surrounding text");
    }

    #[test]
    fn missing_argument_is_e003() {
        let scope = scope_of(&[]);
        let err = resolve(&scan("$nope", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E003);
        assert!(err.message.contains("nope"), "{}", err.message);

        let err = resolve(&scan("x $3", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E003);
        assert!(err.message.contains("$3"), "{}", err.message);
    }

    #[test]
    fn named_argument_lookup_returns_value_or_e003() {
        // (a) named argument in scope wins.
        let scope = Scope {
            named: named(&[("x", Value::int(1))]),
            ..Scope::new()
        };
        assert_eq!(
            resolve(&scan("$x", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::int(1)
        );

        // (b) missing named argument is E003 (no component fallback).
        let scope = Scope::new();
        let err = resolve(&scan("$comp", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E003);

        // Positional `$N` references never return a component value.
        let scope = Scope::new();
        let err = resolve(&scan("$0", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E003);
    }

    #[test]
    fn missing_named_argument_in_surrounding_text_is_e003() {
        let scope = Scope::new();
        let err = resolve(&scan("r=$ratio!", SPAN).unwrap(), &scope, &FakeEngine).unwrap_err();
        assert_eq!(err.code, E003);
        assert!(err.message.contains("ratio"), "{}", err.message);
    }

    #[test]
    fn math_segment_defers_to_engine() {
        let scope = scope_of(&[]);
        assert_eq!(
            resolve(&scan("${42}", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::int(42)
        );
        assert_eq!(
            resolve(&scan("${7}", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::int(7),
            "single math segment keeps the engine's native type"
        );
        assert_eq!(
            resolve(&scan("v=${8}!", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("v=8!")
        );
    }

    #[test]
    fn math_segment_evaluates_with_v1_engine() {
        use crate::math::V1Engine;
        let scope = Scope::with_args(vec![], vec![Value::int(12), Value::int(34)]);
        assert_eq!(
            resolve(&scan("${$0 + $1}", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::int(46)
        );
        let scope = Scope {
            call: Some(Rc::new(|name: &str, args: &[Value]| {
                let sum: i64 = args
                    .iter()
                    .map(|v| match v {
                        Value::Int(i) => *i,
                        _ => 0,
                    })
                    .sum();
                Ok(match name {
                    "b" => Value::int(sum),
                    "c" => Value::int(2 * sum),
                    _ => Value::int(0),
                })
            })),
            shell_call: Some(Rc::new(|call, _scope, _span| {
                let sum: i64 = call
                    .args
                    .iter()
                    .map(|arg| match &arg.value {
                        callsite::ParsedValue::Literal(Value::Int(i)) => *i,
                        _ => 0,
                    })
                    .sum();
                let value = match call.name.as_str() {
                    "b" => Value::int(sum),
                    "c" => Value::int(2 * sum),
                    _ => Value::int(0),
                };
                Ok(value)
            })),
            ..Scope::new()
        };
        assert_eq!(
            resolve(
                &scan("${b(12,34) + c(28)}", SPAN).unwrap(),
                &scope,
                &V1Engine
            )
            .unwrap(),
            Value::int(102)
        );
    }

    #[test]
    fn rescanned_last_in_math_segment() {
        use crate::math::V1Engine;
        let scope = Scope::reduce_step(vec![], vec![], Value::string("1 + 2"));
        assert_eq!(
            resolve(&scan("${last}", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::int(3)
        );
        let scope = Scope::with_args(vec![("a".to_string(), Value::string("free text"))], vec![]);
        let err = resolve(&scan("${a * 2}", SPAN).unwrap(), &scope, &V1Engine).unwrap_err();
        assert_eq!(
            err.code, E011,
            "non-parseable string left as-is then numeric op"
        );
    }

    #[test]
    fn dollar_last_outside_reduce_is_e003() {
        use crate::math::V1Engine;
        let err = resolve(&scan("v: $last", SPAN).unwrap(), &Scope::new(), &V1Engine).unwrap_err();
        assert_eq!(err.code, E003);
        assert!(err.message.contains("last"), "{}", err.message);
        let scope = Scope::with_args(vec![("x".to_string(), Value::int(1))], vec![]);
        let err = resolve(&scan("$last", SPAN).unwrap(), &scope, &V1Engine).unwrap_err();
        assert_eq!(err.code, E003);
    }

    #[test]
    fn dollar_last_preserves_native_type() {
        use crate::math::V1Engine;
        let scope = Scope::reduce_step(vec![], vec![], Value::bool(true));
        assert_eq!(
            resolve(&scan("$last", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::bool(true)
        );
        let scope = Scope::reduce_step(vec![], vec![], Value::float(2.0));
        assert_eq!(
            resolve(&scan("v: $last", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::string("v: 2.0")
        );
    }

    #[test]
    fn math_segment_returns_any_value() {
        use crate::math::V1Engine;
        let obj = obj();
        let arr = Value::array(vec![Value::int(1), Value::int(2)]);
        let scope = Scope::with_args(
            named(&[
                ("obj", obj.clone()),
                ("arr", arr.clone()),
                ("flag", Value::bool(true)),
            ]),
            vec![Value::int(7), obj.clone(), arr.clone()],
        );
        assert_eq!(
            resolve(&scan("${ obj }", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            obj,
            "single math segment returns the Object unchanged"
        );
        assert_eq!(
            resolve(&scan("${ arr }", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            arr
        );
        assert_eq!(
            resolve(&scan("${ flag }", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::bool(true)
        );
        assert_eq!(
            resolve(&scan("${ $0 }", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::int(7),
            "dollar-zero returns the argument unchanged"
        );
        assert_eq!(
            resolve(&scan("${ $1 }", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            obj
        );
    }

    #[test]
    fn math_string_concat_flows_as_string() {
        use crate::math::V1Engine;
        let scope = scope_of(&[("x", Value::string("n=")), ("y", Value::int(5))]);
        assert_eq!(
            resolve(&scan("${ x + y }", SPAN).unwrap(), &scope, &V1Engine).unwrap(),
            Value::string("n=5"),
            "String concat inside math flows into single interpolation as String"
        );
    }

    #[test]
    fn plain_text_round_trips() {
        let scope = scope_of(&[]);
        assert_eq!(
            resolve(&scan("no dollars here", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("no dollars here")
        );
        assert_eq!(
            resolve(&scan("", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("")
        );
    }

    #[test]
    fn scan_reports_segment_spans() {
        let segs = scan("ab$cd", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![
                Segment::Text("ab".to_string()),
                Segment::Arg {
                    name: "cd".to_string(),
                    span: Span { line: 1, col: 3 },
                },
            ]
        );
        let segs = scan("$m ${x}", SPAN).unwrap();
        assert_eq!(
            segs[0],
            Segment::Arg {
                name: "m".to_string(),
                span: SPAN
            }
        );
        assert_eq!(segs[1], Segment::Text(" ".to_string()));
        assert_eq!(
            segs[2],
            Segment::Math {
                src: "x".to_string(),
                span: Span { line: 1, col: 4 },
            }
        );
    }

    #[test]
    fn scan_advances_lines_across_newlines() {
        let segs = scan("x\ny $z", SPAN).unwrap();
        assert_eq!(segs[0], Segment::Text("x\ny ".to_string()));
        assert_eq!(
            segs[1],
            Segment::Arg {
                name: "z".to_string(),
                span: Span { line: 2, col: 3 },
            }
        );
    }

    #[test]
    fn brace_call_scans_correctly() {
        let segs = scan("$sh{echo hi}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "sh".to_string(),
                payload_src: "echo hi".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_pw_scans_correctly() {
        let segs = scan("$pw{Get-Content x}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "pw".to_string(),
                payload_src: "Get-Content x".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_with_interpolation_in_payload() {
        let segs = scan("$sh{echo $name}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "sh".to_string(),
                payload_src: "echo $name".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_with_surrounding_text() {
        let segs = scan("v=$sh{echo hi}!", SPAN).unwrap();
        assert_eq!(segs[0], Segment::Text("v=".to_string()));
        assert_eq!(
            segs[1],
            Segment::BraceCall {
                name: "sh".to_string(),
                payload_src: "echo hi".to_string(),
                span: Span { line: 1, col: 3 },
            }
        );
        assert_eq!(segs[2], Segment::Text("!".to_string()));
    }

    #[test]
    fn math_takes_precedence_over_brace_call() {
        // `${sh{...}}` — math containing brace call — is NOT confused
        // with `$sh{...}`. The `${` takes precedence: the scanner enters
        // math mode and reads until the first `}`, so `${sh{echo hi}}`
        // produces Math{src:"sh{echo hi"} + Text("}").
        let segs = scan("${sh{echo hi}}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![
                Segment::Math {
                    src: "sh{echo hi".to_string(),
                    span: SPAN,
                },
                Segment::Text("}".to_string()),
            ]
        );
    }

    #[test]
    fn brace_call_with_underscore_name() {
        let segs = scan("$_sh{echo hi}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "_sh".to_string(),
                payload_src: "echo hi".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_unterminated_is_e010() {
        let err = scan("$sh{echo hi", SPAN).unwrap_err();
        assert_eq!(err.code, E010);
        assert!(err.message.contains("unterminated"), "{}", err.message);
    }

    #[test]
    fn brace_call_empty_payload() {
        let segs = scan("$sh{}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "sh".to_string(),
                payload_src: "".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_balanced_payload() {
        // Balanced, quote-aware payload: `{$a: 1, b: $c}` captures the full object
        let segs = scan("$b{{c: 1, d: 2}}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "b".to_string(),
                payload_src: "{c: 1, d: 2}".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_preserves_quoted_braces_in_payload() {
        // `$sh{awk '{print $1}'}` should capture the full command including quotes
        let segs = scan(r#"$sh{awk '{print $1}'}"#, SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "sh".to_string(),
                payload_src: "awk '{print $1}'".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn brace_call_nested_braces_in_payload() {
        // A brace call with nested braces: `$x{{a: 1}, {b: 2}}`
        let segs = scan("$x{{a: 1}, {b: 2}}", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![Segment::BraceCall {
                name: "x".to_string(),
                payload_src: "{a: 1}, {b: 2}".to_string(),
                span: SPAN,
            }]
        );
    }

    #[test]
    fn shell_component_call_scans_and_resolves() {
        let scope = Scope {
            shell_call: Some(Rc::new(|call, _scope, _span| {
                let sum: i64 = call
                    .args
                    .iter()
                    .map(|arg| match &arg.value {
                        crate::callsite::ParsedValue::Literal(Value::Int(i)) => *i,
                        _ => 0,
                    })
                    .sum();
                Ok(match call.name.as_str() {
                    "sum" => Value::int(sum),
                    "noop" => Value::int(0),
                    _ => Value::int(-1),
                })
            })),
            ..Scope::new()
        };

        let segs = scan_shell("echo $sum(1,2)", SPAN).unwrap();
        assert_eq!(
            segs,
            vec![
                Segment::Text("echo ".to_string()),
                Segment::Call {
                    name: "sum".to_string(),
                    args: "1,2".to_string(),
                    span: Span { line: 1, col: 6 },
                },
            ]
        );
        assert_eq!(
            resolve_shell(&segs, &scope, &FakeEngine).unwrap(),
            Value::string("echo 3")
        );
        assert_eq!(
            resolve_shell(
                &scan_shell("x $sum(a=1, b=2)", SPAN).unwrap(),
                &scope,
                &FakeEngine
            )
            .unwrap(),
            Value::string("x 3")
        );
        assert_eq!(
            resolve_shell(&scan_shell("x $noop()", SPAN).unwrap(), &scope, &FakeEngine).unwrap(),
            Value::string("x 0")
        );
    }
}
