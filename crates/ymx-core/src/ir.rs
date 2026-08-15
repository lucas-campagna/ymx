//! Core intermediate representation for the YMX compiler.
//!
//! [`Value`] is the single IR produced by resolving a YMX document and the
//! type that `serde_json::to_string` consumes to emit JSON output. Object
//! keys preserve YAML insertion order via [`IndexMap`]; combined with the
//! `serde_json` `preserve_order` feature, serialized object keys appear in
//! insertion order rather than lexicographically.

use indexmap::IndexMap;
use serde::Serialize;

/// The YMX intermediate representation.
///
/// Serialized as a plain JSON value via `#[serde(untagged)]` (PRD: "YAML ->
/// intermediate `Value` IR -> serialize to JSON"): `Null` -> `null`,
/// `Bool(true)` -> `true`, `Int(5)` -> `5`, `Float(2.0)` -> `2.0`,
/// `String("x")` -> `"x"`, `Array` -> `[...]`, `Object` -> `{...}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    Object(IndexMap<String, Value>),
}

impl Value {
    pub fn null() -> Value {
        Value::Null
    }

    pub fn bool(b: bool) -> Value {
        Value::Bool(b)
    }

    pub fn int(i: i64) -> Value {
        Value::Int(i)
    }

    pub fn float(f: f64) -> Value {
        Value::Float(f)
    }

    pub fn string<S: Into<String>>(s: S) -> Value {
        Value::String(s.into())
    }

    pub fn array(v: Vec<Value>) -> Value {
        Value::Array(v)
    }

    pub fn object(m: IndexMap<String, Value>) -> Value {
        Value::Object(m)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// `true` for leaf values (`Null` / `Bool` / `Int` / `Float` / `String`);
    /// `false` for containers (`Array` / `Object`).
    pub fn is_scalar(&self) -> bool {
        !matches!(self, Value::Array(_) | Value::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }
}

/// Call arguments (named and/or positional) for `compile_component`.
///
/// `TestArgs` (the per-test target analogue) lives in `ymx-test`, not here.
#[derive(Debug, Clone, PartialEq)]
pub enum Args {
    None,
    Named(Vec<(String, Value)>),
    Positional(Vec<Value>),
    Mixed {
        named: Vec<(String, Value)>,
        positional: Vec<Value>,
    },
}

/// Single shared f64 renderer used for **both** JSON output of `Value::Float`
/// and `${...}` string interpolation.
///
/// Rust's default `{}` formatting of `f64` drops the fractional part of
/// integer-valued floats (`2.0_f64` -> `"2"`), which would violate the YMX IR
/// contract that integer-valued floats keep their fractional part. This
/// renderer uses `ryu`, which emits the shortest round-trippable decimal
/// representation and always retains a decimal point for finite values:
/// `2.0` -> `"2.0"`, `0.1` -> `"0.1"`, `2.5` -> `"2.5"`.
///
/// (Architecture invariant #7: a single shared f64 renderer is used for JSON
/// output and string interpolation; Rust's default `{}` formatting is
/// intentionally **not** used.)
pub fn render_f64(value: f64) -> String {
    let mut buf = ryu::Buffer::new();
    buf.format(value).to_owned()
}

/// A value with no string rendering (an Array or an Object). [`render_value`]
/// rejects these; callers raise `E011`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoStringRender;

/// Single shared scalar-to-text renderer used for **both** string
/// interpolation and math `+` string-concatenation (PRD *Number→string
/// rendering*).
///
/// Int renders plainly (`20` → `"20"`), Float renders through [`render_f64`]
/// (integer-valued floats keep their fractional part — `2.0` → `"2.0"`), Bool
/// renders `"true"` / `"false"`, Null renders `"null"`, and String passes
/// through unchanged. Objects and arrays have no meaningful string rendering
/// (PRD *String syntax*) and are rejected with
/// [`Err(NoStringRender)`](NoStringRender) — callers raise `E011`. Rust's
/// default `{}` formatting is intentionally **not** used for floats
/// (invariant #7).
pub fn render_value(v: &Value) -> Result<String, NoStringRender> {
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
        Value::Array(_) | Value::Object(_) => Err(NoStringRender),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_serializes_in_insertion_order() {
        let mut m = IndexMap::new();
        m.insert("zebra".to_string(), Value::int(1));
        m.insert("apple".to_string(), Value::int(2));
        m.insert("mango".to_string(), Value::int(3));
        let v = Value::object(m);
        let s = serde_json::to_string(&v).expect("serialize");
        assert_eq!(s, r#"{"zebra":1,"apple":2,"mango":3}"#);
    }

    #[test]
    fn scalar_and_container_predicates() {
        assert!(Value::null().is_null());
        assert!(Value::null().is_scalar());
        assert!(!Value::null().is_array());
        assert!(!Value::null().is_object());

        assert!(Value::bool(true).is_scalar());
        assert!(Value::int(5).is_scalar());
        assert!(Value::float(1.5).is_scalar());
        assert!(Value::string("x").is_scalar());
        assert!(!Value::string("x").is_null());

        assert!(Value::array(vec![]).is_array());
        assert!(!Value::array(vec![]).is_scalar());

        assert!(Value::object(IndexMap::new()).is_object());
        assert!(!Value::object(IndexMap::new()).is_scalar());
    }

    #[test]
    fn render_f64_keeps_fractional_part() {
        assert_eq!(render_f64(2.0_f64), "2.0");
        assert_eq!(render_f64(0.1), "0.1");
        assert_eq!(render_f64(2.5), "2.5");
        assert_eq!(render_f64(3.0_f64), "3.0");
    }

    #[test]
    fn render_value_renders_scalars_to_text() {
        assert_eq!(render_value(&Value::Int(20)).unwrap(), "20");
        assert_eq!(render_value(&Value::Int(-7)).unwrap(), "-7");
        assert_eq!(render_value(&Value::Float(2.0)).unwrap(), "2.0");
        assert_eq!(render_value(&Value::Float(2.5)).unwrap(), "2.5");
        assert_eq!(render_value(&Value::Float(0.1)).unwrap(), "0.1");
        assert_eq!(render_value(&Value::Bool(true)).unwrap(), "true");
        assert_eq!(render_value(&Value::Bool(false)).unwrap(), "false");
        assert_eq!(render_value(&Value::Null).unwrap(), "null");
        assert_eq!(render_value(&Value::string("x")).unwrap(), "x");
    }

    #[test]
    fn render_value_rejects_containers() {
        assert!(render_value(&Value::Array(vec![])).is_err());
        assert!(render_value(&Value::Object(IndexMap::new())).is_err());
    }

    #[test]
    fn value_serializes_as_plain_json() {
        assert_eq!(serde_json::to_string(&Value::Null).unwrap(), "null");
        assert_eq!(serde_json::to_string(&Value::Bool(true)).unwrap(), "true");
        assert_eq!(serde_json::to_string(&Value::Bool(false)).unwrap(), "false");
        assert_eq!(serde_json::to_string(&Value::Int(5)).unwrap(), "5");
        assert_eq!(serde_json::to_string(&Value::Int(-7)).unwrap(), "-7");
        assert_eq!(serde_json::to_string(&Value::Float(2.0)).unwrap(), "2.0");
        assert_eq!(
            serde_json::to_string(&Value::string("hi")).unwrap(),
            r#""hi""#
        );
        assert_eq!(
            serde_json::to_string(&Value::array(vec![Value::int(1), Value::int(2)])).unwrap(),
            "[1,2]"
        );
        let mut m = IndexMap::new();
        m.insert("k".to_string(), Value::int(1));
        assert_eq!(
            serde_json::to_string(&Value::object(m)).unwrap(),
            r#"{"k":1}"#
        );
    }
}
