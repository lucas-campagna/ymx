use indexmap::IndexMap;
use serde_json;

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
    "defer",
];

/// Lowercase set of HTML void (self-closing) elements that have no closing tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Case-insensitive set of known HTML attribute names.
/// These keys will never be treated as tag names in the `from`-key shortcut.
const KNOWN_HTML_ATTRS: &[&str] = &[
    // Reserved/meta keys
    "style",
    "class",
    "children",
    "from",
    // Boolean attributes (already in BOOLEAN_ATTRS but listed here too for completeness)
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
    // General HTML attributes
    "id",
    "name",
    "type",
    "value",
    "placeholder",
    "title",
    "alt",
    "src",
    "href",
    "action",
    "method",
    "target",
    "rel",
    "media",
    "width",
    "height",
    "size",
    "max",
    "min",
    "step",
    "pattern",
    "async",
    "defer",
    "crossorigin",
    "integrity",
    "nomodule",
    "scoped",
    // data-* attributes
    "data-id",
    "data-key",
    "data-value",
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
            } else if let Some((tag_key, tag_val)) = find_tag_shortcut(map) {
                let mut m = map.clone();
                m.shift_remove(tag_key);
                m.insert("from".into(), Value::String(tag_key.to_string()));
                m.insert("children".into(), tag_val.clone());
                render_tag(&m)
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
    let raw_tag = map
        .get("from")
        .and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
        .unwrap_or("div");

    let tag = if raw_tag.starts_with('<') && raw_tag.ends_with('>') && raw_tag.len() > 2 {
        &raw_tag[1..raw_tag.len() - 1]
    } else {
        raw_tag
    };

    let mut attrs = render_attrs(map);

    // Determine inner content and any additional attrs from nested children object
    let inner = if let Some(Value::Object(children_obj)) = map.get("children") {
        // If children value is an object with a `children` key AND no `from` key,
        // extract inner children and use the remaining keys as additional attributes.
        // If the children object has `from`, it's a nested tag — render normally.
        if children_obj.get("children").is_some() && !children_obj.contains_key("from") {
            let inner_children = children_obj.get("children").unwrap();
            // Build extra attr string for the outer tag from the children object's other keys
            let extra_attrs =
                render_attrs_from_map(children_obj, Some(&["children".into(), "from".into()]));
            if !extra_attrs.is_empty() {
                attrs.push_str(&extra_attrs);
            }
            // Render the inner children value
            render_value(inner_children)
        } else {
            // Either no `children` key, or has `from` — treat as normal nested tag
            render_value(&Value::Object(children_obj.clone()))
        }
    } else {
        // Children is not an object (scalar, array, etc.)
        map.get("children").map(render_value).unwrap_or_default()
    };

    if inner.is_empty() && is_void_element(tag) {
        format!("<{tag}{attrs}>")
    } else {
        format!("<{tag}{attrs}>{inner}</{tag}>")
    }
}

/// Render attributes from an object map, optionally skipping certain keys.
fn render_attrs_from_map(
    map: &IndexMap<String, Value>,
    skip_keys: Option<&[std::borrow::Cow<'_, str>]>,
) -> String {
    let mut out = String::new();

    for (key, val) in map {
        if key == "from" || key == "children" {
            continue;
        }
        if let Some(skip) = skip_keys {
            if skip.iter().any(|k| k.eq_ignore_ascii_case(key)) {
                continue;
            }
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

/// Render an object without `from` as text content for each key-value pair.
/// Scalar-valued known HTML attribute keys are rendered as `key="value"` attributes.
fn render_object_text(map: &IndexMap<String, Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut attr_parts: Vec<String> = Vec::new();

    for (key, val) in map {
        if is_known_html_attr(key) && is_scalar_val(val) {
            attr_parts.push(format!("{}=\"{}\"", key, stringify_attr_value(val)));
        } else if is_known_html_attr(key) {
            // Non-scalar value for a known HTML attr (e.g. data-config with object value)
            // stringify_attr_value handles arrays/objects
            attr_parts.push(format!("{}=\"{}\"", key, stringify_attr_value(val)));
        } else {
            match val {
                Value::Bool(true) => {
                    // Boolean attribute with true value - render as bare attribute
                    attr_parts.push(key.clone());
                }
                Value::Bool(false) | Value::Null => {
                    // Skip false booleans and nulls
                }
                _ => {
                    parts.push(render_value(val));
                }
            }
        }
    }

    let mut out = attr_parts.join(" ");
    if !parts.is_empty() {
        out.push_str(&parts.join(""));
    }
    out
}

/// Return `true` if `key` looks like a framework/directive attribute rather than a tag name.
/// Matches patterns like: `x-data`, `@click`, `x-show`, `hx-target`, `:value`, etc.
fn looks_like_attr(key: &str) -> bool {
    let k = key.as_bytes();
    // @click, @submit, @change, etc. (event handlers)
    k.first() == Some(&b'@')
    // x-data, x-show, x-model, x-bind, x-for, x-if, x-scope, etc. (Alpine/livewire)
    || (k.first() == Some(&b'x') && k.get(1) == Some(&b'-'))
    // :value, :disabled, etc. (Vue bindings / Alpine x-bind shorthand)
    || (k.first() == Some(&b':') && k.len() > 1)
    // hx-target, hx-post, etc. (HTMX attributes)
    || key.starts_with("hx-")
    // v-if, v-for, v-show, v-model, etc. (Vue directives)
    || (k.first() == Some(&b'v') && k.get(1) == Some(&b'-'))
}

/// Find a single key that is a likely HTML tag name.
/// Keys that look like framework attributes (@, x-, :, hx-, v-) are excluded.
/// If the object has a `children` key plus exactly one other non-attribute key,
/// the other key is treated as the tag name.
fn find_tag_shortcut(map: &IndexMap<String, Value>) -> Option<(&str, &Value)> {
    let non_attr: Vec<_> = map
        .iter()
        .filter(|(k, _)| !is_known_html_attr(k) && !k.eq_ignore_ascii_case("children"))
        .collect();

    // If object has `children` plus exactly one other non-attribute key, that other key is the tag
    if map.contains_key("children") && non_attr.len() == 1 {
        let (key, val) = non_attr[0];
        return Some((key.as_str(), val));
    }

    // If exactly one non-attribute key (existing behavior)
    if non_attr.len() == 1 {
        let (key, val) = non_attr[0];
        return Some((key.as_str(), val));
    }

    // If multiple non-attribute keys, check if one is a tag and others are booleans
    // A key is a potential tag if its value is NOT boolean and NOT array and key doesn't look like an attr
    let tag_candidates: Vec<_> = non_attr
        .iter()
        .filter(|(k, v)| !matches!(v, Value::Bool(_) | Value::Array(_)) && !looks_like_attr(k))
        .collect();

    // If exactly one tag candidate AND all other non-attr keys are booleans or attr-like
    if tag_candidates.len() == 1 && non_attr.len() > 1 {
        let remaining_all_ok = non_attr
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), _ if *k == tag_candidates[0].0))
            .all(|(k, v)| matches!(v, Value::Bool(_)) || looks_like_attr(k));

        if remaining_all_ok {
            let (key, val) = tag_candidates[0];
            return Some((key.as_str(), val));
        }
    }

    None
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
                out.push(' ');
                out.push_str(key);
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

/// Return `true` if `key` is a known HTML attribute name (case-insensitive).
/// Also returns `true` for any key starting with `data-`.
fn is_known_html_attr(key: &str) -> bool {
    let lower = key.to_lowercase();
    if lower.starts_with("data-") {
        return true;
    }
    if KNOWN_HTML_ATTRS.iter().any(|&k| lower == k) {
        return true;
    }
    // Also recognize framework attribute prefixes (x-, @, :, hx-, v-)
    looks_like_attr(key)
}

/// Return `true` if `tag` is a known HTML void (self-closing) element (case-insensitive).
fn is_void_element(tag: &str) -> bool {
    VOID_ELEMENTS.iter().any(|&e| tag.eq_ignore_ascii_case(e))
}

/// Return `true` if `v` is a scalar value (string/number/bool/null, not array/object).
fn is_scalar_val(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
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
        Value::Array(arr) => serde_json::to_string(arr)
            .map(|s| s.replace('"', "'"))
            .unwrap_or_else(|_| {
                let items: Vec<String> = arr.iter().map(stringify_attr_value).collect();
                items.join(" ")
            }),
        Value::Object(obj) => serde_json::to_string(obj)
            .map(|s| s.replace('"', "'"))
            .unwrap_or_else(|_| "{...}".to_string()),
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

/// Pretty-print an HTML string with proper indentation.
pub fn pretty_print_html(html: &str) -> String {
    let void_elements: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];

    let mut result = String::new();
    let mut depth: usize = 0;
    let mut in_tag = false;
    let mut current_tag = String::new();
    // Stack to track if each open tag had child tags
    let mut had_children_stack: Vec<bool> = Vec::new();

    let chars = html.chars();

    for c in chars {
        match c {
            '<' => {
                in_tag = true;
                current_tag.clear();
                current_tag.push(c);
            }
            '>' if in_tag => {
                in_tag = false;
                current_tag.push(c);

                let is_closing = current_tag.starts_with("</");
                let is_void = {
                    let inner = current_tag
                        .trim_start_matches("<")
                        .trim_start_matches("</")
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    void_elements.contains(&inner)
                };

                if is_closing {
                    // Remove trailing whitespace before closing tag
                    while result.ends_with('\n') || result.ends_with(' ') {
                        result.pop();
                    }
                    // Check if this tag had children (pop from stack)
                    let had_children = had_children_stack.pop().unwrap_or(false);
                    if had_children {
                        // Use depth-1 for indent since we're closing the current tag
                        result.push('\n');
                        result.push_str(&"  ".repeat(depth.saturating_sub(1)));
                    }
                    result.push_str(&current_tag);
                    depth = depth.saturating_sub(1);
                } else {
                    // Remove trailing whitespace before opening tag
                    while result.ends_with('\n') || result.ends_with(' ') {
                        result.pop();
                    }
                    result.push('\n');
                    result.push_str(&"  ".repeat(depth));
                    result.push_str(&current_tag);
                    // If we're already inside a tag (depth > 0), then this new tag
                    // is a child, so mark the parent as having children
                    if depth > 0 {
                        if let Some(parent_had_children) = had_children_stack.last_mut() {
                            *parent_had_children = true;
                        }
                    }
                    if !is_void {
                        depth += 1;
                    }
                    // New tag hasn't had children yet
                    had_children_stack.push(false);
                }
            }
            _ => {
                if !in_tag {
                    // Text content - just append
                    result.push(c);
                } else {
                    current_tag.push(c);
                }
            }
        }
    }

    while result.ends_with('\n') || result.ends_with(' ') {
        result.pop();
    }
    result.push('\n');

    result
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
        assert_eq!(normalize_class(&Value::string("a  b  c")), "a b c");
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
        assert_eq!(stringify_attr_value(&arr), "['a','b']"); // JSON array
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

    #[test]
    fn from_key_shortcut_single_scalar_renders_as_tag() {
        // `button: click-me` → `<button>click-me</button>`
        let mut map = IndexMap::new();
        map.insert("button".into(), Value::string("click-me"));
        assert_eq!(
            render_value(&Value::Object(map)),
            "<button>click-me</button>"
        );
    }

    #[test]
    fn from_key_shortcut_with_class_attribute() {
        // `button: click-me` with `class: "btn"` → `<button class="btn">click-me</button>`
        let mut map = IndexMap::new();
        map.insert("button".into(), Value::string("click-me"));
        map.insert("class".into(), Value::string("btn"));
        assert_eq!(
            render_value(&Value::Object(map)),
            "<button class=\"btn\">click-me</button>"
        );
    }

    #[test]
    fn from_key_shortcut_href_is_attribute_not_tag() {
        // `href: "url"` should render as attribute href="url", not as a tag
        let mut map = IndexMap::new();
        map.insert("href".into(), Value::string("url"));
        let result = render_value(&Value::Object(map));
        assert!(!result.starts_with("<href"));
        assert!(result.contains("href=\"url\""));
    }

    #[test]
    fn from_key_shortcut_data_attr_is_attribute_not_tag() {
        // `data-id: "123"` should render as attribute, not as a tag
        let mut map = IndexMap::new();
        map.insert("data-id".into(), Value::string("123"));
        let result = render_value(&Value::Object(map));
        assert!(!result.starts_with("<data-id"));
        assert!(result.contains("data-id=\"123\""));
    }

    #[test]
    fn from_key_shortcut_multiple_scalar_keys_not_a_shortcut() {
        // Multiple scalar keys without `from` — no shortcut fires
        let mut map = IndexMap::new();
        map.insert("prop_a".into(), Value::string("val_a"));
        map.insert("prop_b".into(), Value::string("val_b"));
        let result = render_value(&Value::Object(map));
        // Falls back to object-text: values concatenated
        assert_eq!(result, "val_aval_b");
    }

    #[test]
    fn from_key_shortcut_with_style_object() {
        let mut map = IndexMap::new();
        map.insert("div".into(), Value::string("content"));
        let mut style = IndexMap::new();
        style.insert("color".into(), Value::string("red"));
        map.insert("style".into(), Value::Object(style));
        let result = render_value(&Value::Object(map));
        assert_eq!(result, "<div style=\"color: red\">content</div>");
    }

    #[test]
    fn find_tag_shortcut_accepts_array_value() {
        let mut map = IndexMap::new();
        map.insert(
            "body".into(),
            Value::array(vec![Value::string("a"), Value::string("b")]),
        );
        let result = find_tag_shortcut(&map);
        assert!(result.is_some());
        let (tag, val) = result.unwrap();
        assert_eq!(tag, "body");
        assert!(matches!(val, Value::Array(_)));
    }

    #[test]
    fn find_tag_shortcut_rejects_known_attr() {
        let mut map = IndexMap::new();
        map.insert("id".into(), Value::string("my-id"));
        assert!(find_tag_shortcut(&map).is_none());
    }

    #[test]
    fn find_tag_shortcut_accepts_valid_tag() {
        let mut map = IndexMap::new();
        map.insert("my_button".into(), Value::string("click"));
        let result = find_tag_shortcut(&map);
        assert!(result.is_some());
        let (tag, val) = result.unwrap();
        assert_eq!(tag, "my_button");
        assert_eq!(val, &Value::string("click"));
    }
}
