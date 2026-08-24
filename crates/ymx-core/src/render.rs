use indexmap::IndexMap;

use crate::ir::{render_f64, Value};

/// Trait for rendering a [`Value`] tree to HTML.
pub trait HtmlRenderer {
    fn render_html(&self, value: &Value) -> String;
}

/// Default HTML renderer.
pub struct DefaultHtmlRenderer;

/// Case-insensitive set of HTML boolean attributes.
const BOOLEAN_ATTRS: &[&str] = &[
    "disabled",
    "readonly",
    "checked",
    "selected",
    "multiple",
    "autofocus",
    "autocomplete",
    "hidden",
    "controls",
    "loop",
    "muted",
    "playsinline",
    "autoplay",
    "preload",
    "required",
    "novalidate",
    "formnovalidate",
    "reversed",
    "indeterminate",
];

impl HtmlRenderer for DefaultHtmlRenderer {
    fn render_html(&self, value: &Value) -> String {
        render_value(value)
    }
}

/// Recursively render a `Value` to an HTML string (inner-first).
fn render_value(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) => {
            render_scalar(v)
        }
        Value::Array(arr) => arr.iter().map(render_value).collect(),
        Value::Object(map) => {
            if map.contains_key("from") {
                render_tag(map)
            } else {
                render_object_text(map)
            }
        }
    }
}

/// Render a scalar value as HTML-escaped text content.
fn render_scalar(v: &Value) -> String {
    let text = match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => render_f64(*f),
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    html_escape(&text)
}

/// Render an object with a `from` key as an HTML element.
fn render_tag(map: &IndexMap<String, Value>) -> String {
    let tag = map
        .get("from")
        .and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
        .unwrap_or("div");

    let attrs = render_attrs(map);

    let inner = map
        .get("children")
        .map(render_value)
        .unwrap_or_default();

    format!("<{tag}{attrs}>{inner}</{tag}>")
}

/// Render an object without `from` as text content for each key-value pair.
fn render_object_text(map: &IndexMap<String, Value>) -> String {
    map.values().map(render_value).collect()
}

/// Render HTML attributes from an object map, skipping `from` and `children`.
fn render_attrs(map: &IndexMap<String, Value>) -> String {
    let mut out = String::new();

    for (key, val) in map {
        if key == "from" || key == "children" {
            continue;
        }

        match val {
            Value::Bool(false) | Value::Null => continue,
            Value::String(s) if s.is_empty() => continue,
            Value::Bool(true) => {
                if is_boolean_attr(key) {
                    out.push(' ');
                    out.push_str(key);
                } else {
                    out.push(' ');
                    out.push_str(key);
                    out.push_str("=\"true\"");
                }
            }
            _ => {
                let normalized = match key.as_str() {
                    "style" => normalize_style(val),
                    "class" => normalize_class(val),
                    _ => stringify_attr_value(val),
                };
                if !normalized.is_empty() {
                    out.push(' ');
                    out.push_str(key);
                    out.push_str("=\"");
                    out.push_str(&normalized);
                    out.push('"');
                }
            }
        }
    }

    out
}

/// Return `true` if `key` matches a known boolean HTML attribute (case-insensitive).
fn is_boolean_attr(key: &str) -> bool {
    let lower = key.to_lowercase();
    BOOLEAN_ATTRS.iter().any(|&b| lower == b)
}

/// Normalize a `style` value to a CSS attribute string.
///
/// - **String**: returned as-is.
/// - **Object**: each pair becomes `"key: value"`, joined by `"; "`.
/// - **Other**: empty string.
pub fn normalize_style(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .iter()
            .map(|(k, val)| format!("{k}: {}", stringify_attr_value(val)))
            .collect::<Vec<_>>()
            .join("; "),
        _ => String::new(),
    }
}

/// Normalize a `class` value to a space-separated class string.
///
/// - **String**: split on whitespace, filter empty, rejoin.
/// - **Array**: each element stringified, filtered, rejoin.
/// - **Object**: keys where value is truthy, rejoin.
/// - **Other**: empty string.
pub fn normalize_class(v: &Value) -> String {
    match v {
        Value::String(s) => s
            .split_whitespace()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Value::Array(arr) => arr
            .iter()
            .map(stringify_attr_value)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(map) => map
            .iter()
            .filter(|(_, val)| is_truthy(val))
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Stringify a value for use in an HTML attribute.
pub fn stringify_attr_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => render_f64(*f),
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .map(stringify_attr_value)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(_) => "{...}".to_string(),
    }
}

/// Return `true` if a value is considered truthy for `normalize_class`.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// Escape HTML special characters in text content.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Value;

    #[test]
    fn scalar_renders_as_escaped_text() {
        assert_eq!(render_scalar(&Value::Null), "");
        assert_eq!(render_scalar(&Value::Bool(true)), "true");
        assert_eq!(render_scalar(&Value::Int(42)), "42");
        assert_eq!(render_scalar(&Value::Float(2.5)), "2.5");
        assert_eq!(render_scalar(&Value::string("hello")), "hello");
    }

    #[test]
    fn html_escape_special_chars() {
        assert_eq!(html_escape("a<b"), "a&lt;b");
        assert_eq!(html_escape("a&b"), "a&amp;b");
        assert_eq!(html_escape(r#"a"b"#), "a&quot;b");
        assert_eq!(html_escape("a>b"), "a&gt;b");
    }

    #[test]
    fn tag_renders_with_from_key() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("p"));
        map.insert("children".into(), Value::string("hi"));
        assert_eq!(render_tag(&map), "<p>hi</p>");
    }

    #[test]
    fn tag_defaults_to_div() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("div"));
        map.insert("children".into(), Value::string("content"));
        assert_eq!(render_value(&Value::Object(map)), "<div>content</div>");
    }

    #[test]
    fn tag_renders_attributes() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("input"));
        map.insert("type".into(), Value::string("text"));
        map.insert("disabled".into(), Value::Bool(true));
        let result = render_tag(&map);
        assert!(result.starts_with("<input"));
        assert!(result.contains("type=\"text\""));
        assert!(result.contains(" disabled"));
    }

    #[test]
    fn boolean_attr_true_renders_bare() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("input"));
        map.insert("disabled".into(), Value::Bool(true));
        let attrs = render_attrs(&map);
        assert!(attrs.contains(" disabled"));
        assert!(!attrs.contains("disabled=\"true\""));
    }

    #[test]
    fn boolean_attr_false_omitted() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("input"));
        map.insert("disabled".into(), Value::Bool(false));
        let attrs = render_attrs(&map);
        assert!(!attrs.contains("disabled"));
    }

    #[test]
    fn null_attr_omitted() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("input"));
        map.insert("x".into(), Value::Null);
        let attrs = render_attrs(&map);
        assert!(!attrs.contains("x"));
    }

    #[test]
    fn empty_string_attr_omitted() {
        let mut map = IndexMap::new();
        map.insert("from".into(), Value::string("input"));
        map.insert("x".into(), Value::string(""));
        let attrs = render_attrs(&map);
        assert!(!attrs.contains("x"));
    }

    #[test]
    fn normalize_style_string() {
        assert_eq!(normalize_style(&Value::string("color: red")), "color: red");
    }

    #[test]
    fn normalize_style_object() {
        let mut map = IndexMap::new();
        map.insert("color".into(), Value::string("red"));
        map.insert("margin".into(), Value::string("0 auto"));
        let result = normalize_style(&Value::Object(map));
        assert!(result.contains("color: red"));
        assert!(result.contains("margin: 0 auto"));
        assert!(result.contains("; "));
    }

    #[test]
    fn normalize_class_string() {
        assert_eq!(
            normalize_class(&Value::string("a  b  c")),
            "a b c"
        );
    }

    #[test]
    fn normalize_class_array() {
        let arr = Value::array(vec![Value::string("a"), Value::string("b")]);
        assert_eq!(normalize_class(&arr), "a b");
    }

    #[test]
    fn normalize_class_object_truthy() {
        let mut map = IndexMap::new();
        map.insert("active".into(), Value::Bool(true));
        map.insert("hidden".into(), Value::Bool(false));
        map.insert("visible".into(), Value::Bool(true));
        assert_eq!(normalize_class(&Value::Object(map)), "active visible");
    }

    #[test]
    fn stringify_attr_value_array() {
        let arr = Value::array(vec![Value::string("a"), Value::string("b")]);
        assert_eq!(stringify_attr_value(&arr), "a b");
    }

    #[test]
    fn render_value_array_concatenates() {
        let arr = Value::array(vec![Value::string("a"), Value::string("b")]);
        assert_eq!(render_value(&arr), "ab");
    }

    #[test]
    fn render_object_text_concatenates_values() {
        let mut map = IndexMap::new();
        map.insert("a".into(), Value::string("x"));
        map.insert("b".into(), Value::string("y"));
        assert_eq!(render_value(&Value::Object(map)), "xy");
    }

    #[test]
    fn nested_inner_first_rendering() {
        let mut inner = IndexMap::new();
        inner.insert("from".into(), Value::string("span"));
        inner.insert("children".into(), Value::string("inner"));

        let mut outer = IndexMap::new();
        outer.insert("from".into(), Value::string("div"));
        outer.insert("children".into(), Value::Object(inner));

        assert_eq!(
            render_value(&Value::Object(outer)),
            "<div><span>inner</span></div>"
        );
    }

    #[test]
    fn default_html_renderer_trait() {
        let r = DefaultHtmlRenderer;
        let v = Value::string("test");
        assert_eq!(r.render_html(&v), "test");
    }
}
