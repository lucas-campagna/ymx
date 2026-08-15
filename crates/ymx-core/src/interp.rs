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
//! interpoland is rendered per the *Number→string rendering* rule (objects
//! and arrays into text are `E011`).

use crate::diag::{Diagnostic, Span, E003, E010, E011};
use crate::ir::{render_f64, Value};
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
    let mut segments: Vec<Segment> = Vec::new();
    let mut text = String::new();
    let mut chars = src.char_indices().peekable();
    loop {
        let Some((idx, c)) = chars.next() else {
            break;
        };
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
                        segments.push(Segment::Arg {
                            name,
                            span: span_at(base, src, start),
                        });
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
/// interpolations yields a String in which each interpoland is rendered per
/// the *Number→string rendering* rule (Int plain, Float via
/// [`render_f64`](crate::ir::render_f64), Bool `true`/`false`, Null `null`);
/// Objects and Arrays rendered into text are `E011`.
///
/// `${...}` segments are evaluated through `engine` (the [`MathEngine`]
/// boundary); `$name` / `$N` segments resolve against the scope's named /
/// positional arguments (and the reduce-step `last` via
/// [`Scope::lookup`]); a missing argument is `E003`.
pub fn resolve(
    segments: &[Segment],
    scope: &Scope,
    engine: &dyn MathEngine,
) -> Result<Value, Diagnostic> {
    match segments {
        [Segment::Text(t)] => Ok(Value::string(t.clone())),
        [Segment::Arg { name, span }] => resolve_arg(name, *span, scope),
        [Segment::Math { src, .. }] => engine.eval(src, scope),
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
                        let v = engine.eval(src, scope)?;
                        out.push_str(&render_into_text(&v, scope, *span)?);
                    }
                }
            }
            Ok(Value::string(out))
        }
    }
}

/// Resolve a `$name` / `$N` argument reference.
fn resolve_arg(name: &str, span: Span, scope: &Scope) -> Result<Value, Diagnostic> {
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

/// Render an interpolated value into surrounding text (per *Number→string
/// rendering*); Objects and Arrays are `E011`.
fn render_into_text(v: &Value, scope: &Scope, span: Span) -> Result<String, Diagnostic> {
    match render_text(v) {
        Ok(s) => Ok(s),
        Err(()) => Err(ctx_err(
            scope,
            span,
            E011,
            "objects and arrays have no string rendering".to_string(),
        )),
    }
}

/// Number→string rendering: Int plain, Float via [`render_f64`], Bool
/// `true`/`false`, Null `null`, String as-is. Objects and Arrays have no
/// string rendering → `Err(())`.
fn render_text(v: &Value) -> Result<String, ()> {
    match v {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(if *b {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(render_f64(*f)),
        Value::String(s) => Ok(s.clone()),
        Value::Array(_) | Value::Object(_) => Err(()),
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

    const SPAN: Span = Span { line: 1, col: 1 };

    /// Fake engine for task-1 tests (the real v1 evaluator lands with the
    /// math tasks): `${n}` evaluates to the Int literal `n`, else `0`.
    struct FakeEngine;

    impl MathEngine for FakeEngine {
        fn eval(&self, src: &str, _scope: &Scope) -> Result<Value, Diagnostic> {
            Ok(Value::int(src.trim().parse().unwrap_or(0)))
        }
    }

    fn named(entries: &[(&str, Value)]) -> Vec<(String, Value)> {
        entries
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect()
    }

    fn scope_of(entries: &[(&str, Value)]) -> Scope {
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
}
