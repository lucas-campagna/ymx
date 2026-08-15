//! YAML document parsing with source spans.
//!
//! [`parse_document`] parses a single YAML document string into a spanned
//! [`Node`] tree. It is the only place that touches `yaml-rust2`, so the rest
//! of `ymx-core` stays independent of the YAML library. Spans (1-based line,
//! 1-based column) are carried on every node so later stages can attribute
//! diagnostics to the authoring location.
//!
//! v1 rejects, as [`crate::diag::E001`] (`ParseError`):
//!   * multi-document streams (a second `---`);
//!   * complex mapping keys (a mapping or sequence used as a key);
//!   * the YAML merge key `<<`.
//!
//! YAML anchors (`&`) and aliases (`*`) are resolved and inlined; explicit
//! YAML tags (`!!str`, …) are ignored — the scalar's type is inferred from its
//! plain/quoted style and text as if the tag were absent (mirroring
//! `yaml-rust2::yaml::Yaml::from_str`).
//!
//! `ymx-core` is I/O-free, so [`ParseError`] carries no file path; the I/O
//! layer (`ymx-lib::load_project`) attaches the resolved host-file path via
//! [`ParseError::into_diagnostic`].

use std::collections::HashMap;
use std::path::PathBuf;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, ScanError, TScalarStyle};

use crate::diag::{Diagnostic, Span, E001};
use crate::ir::Value;

/// A typed YAML mapping key. Complex keys (mappings/sequences) are rejected at
/// parse time, so a key is always one of the scalar variants. The
/// `Int`/`String` distinction matters for rule 4 (integer positional slots vs.
/// ordinary named properties).
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

/// A single object entry: typed key + key span + value node.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: Key,
    pub key_span: Span,
    pub value: Node,
}

/// A parsed YAML node carrying a 1-based source span. Mirrors [`crate::ir::Value`]
/// but keeps the line/column on every node; the resolver (milestone 1.6) walks
/// this into a span-less [`crate::ir::Value`] after interpolating.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null(Span),
    Bool(bool, Span),
    Int(i64, Span),
    Float(f64, Span),
    String(String, Span),
    Array(Vec<Node>, Span),
    Object(Vec<Entry>, Span),
}

impl Node {
    /// The source span of this node (the location where it begins).
    pub fn span(&self) -> Span {
        match self {
            Node::Null(s)
            | Node::Bool(_, s)
            | Node::Int(_, s)
            | Node::Float(_, s)
            | Node::String(_, s)
            | Node::Array(_, s)
            | Node::Object(_, s) => *s,
        }
    }

    /// `true` for leaf nodes (`Null` / `Bool` / `Int` / `Float` / `String`);
    /// `false` for containers (`Array` / `Object`).
    pub fn is_scalar(&self) -> bool {
        !matches!(self, Node::Array(..) | Node::Object(..))
    }
}

/// A YAML parse / unsupported-feature error. Always carries code [`E001`]; the
/// host-file path is attached by the I/O layer.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl ParseError {
    /// Attach the resolved host-file path and stamp code [`E001`]. Parse errors
    /// are not component-scoped, so `component` is `None`.
    pub fn into_diagnostic(self, file: PathBuf) -> Diagnostic {
        Diagnostic {
            file: Some(file),
            line: self.span.line,
            col: self.span.col,
            component: None,
            code: E001,
            message: self.message,
        }
    }
}

/// Parse one YAML document string into a spanned [`Node`] tree.
///
/// Multi-document streams, complex mapping keys, and the merge key `<<` are
/// rejected with [`ParseError`] (code `E001`). An empty stream parses to
/// `Node::Null` at `1:1`.
pub fn parse_document(src: &str) -> Result<Node, ParseError> {
    let mut parser = Parser::new_from_str(src);
    let mut loader = Loader::default();
    if let Err(scan) = parser.load(&mut loader, true) {
        if loader.error.is_none() {
            loader.error = Some(scan_error(&scan));
        }
    }
    if let Some(err) = loader.error {
        return Err(err);
    }
    Ok(loader.root.unwrap_or(Node::Null(Span { line: 1, col: 1 })))
}

fn scan_error(err: &ScanError) -> ParseError {
    let m = err.marker();
    ParseError {
        span: marker_span(*m),
        message: err.info().to_string(),
    }
}

fn marker_span(m: Marker) -> Span {
    Span {
        line: m.line() as u32,
        col: m.col() as u32 + 1,
    }
}

#[derive(Default)]
struct Loader {
    root: Option<Node>,
    stack: Vec<Frame>,
    anchors: HashMap<usize, Node>,
    error: Option<ParseError>,
    doc_count: usize,
}

enum Frame {
    Array {
        items: Vec<Node>,
        span: Span,
        anchor: usize,
    },
    Map {
        entries: Vec<Entry>,
        span: Span,
        anchor: usize,
        pending_key: Option<(Key, Span)>,
    },
}

impl MarkedEventReceiver for Loader {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        if self.error.is_some() {
            return;
        }
        let span = marker_span(mark);
        match ev {
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
            Event::DocumentStart => {
                self.doc_count += 1;
                if self.doc_count >= 2 {
                    self.error = Some(ParseError {
                        span,
                        message: "multi-document YAML streams (`---`) are not supported"
                            .to_string(),
                    });
                }
            }
            Event::Scalar(v, style, anchor, _tag) => {
                let node = make_scalar(&v, style, span);
                self.complete(node, anchor);
            }
            Event::Alias(id) => {
                let node = match self.anchors.get(&id) {
                    Some(n) => n.clone(),
                    None => {
                        self.error = Some(ParseError {
                            span,
                            message: "unknown YAML alias".to_string(),
                        });
                        return;
                    }
                };
                self.complete(node, 0);
            }
            Event::SequenceStart(anchor, _tag) => {
                if self.key_slot_expecting() {
                    self.error = Some(ParseError {
                        span,
                        message: "complex mapping keys are not supported".to_string(),
                    });
                    return;
                }
                self.stack.push(Frame::Array {
                    items: Vec::new(),
                    span,
                    anchor,
                });
            }
            Event::SequenceEnd => match self.stack.pop() {
                Some(Frame::Array {
                    items,
                    span,
                    anchor,
                }) => {
                    let node = Node::Array(items, span);
                    self.complete(node, anchor);
                }
                _ => unreachable!("SequenceEnd without an Array frame"),
            },
            Event::MappingStart(anchor, _tag) => {
                if self.key_slot_expecting() {
                    self.error = Some(ParseError {
                        span,
                        message: "complex mapping keys are not supported".to_string(),
                    });
                    return;
                }
                self.stack.push(Frame::Map {
                    entries: Vec::new(),
                    span,
                    anchor,
                    pending_key: None,
                });
            }
            Event::MappingEnd => match self.stack.pop() {
                Some(Frame::Map {
                    entries,
                    span,
                    anchor,
                    ..
                }) => {
                    let node = Node::Object(entries, span);
                    self.complete(node, anchor);
                }
                _ => unreachable!("MappingEnd without a Map frame"),
            },
        }
    }
}

impl Loader {
    /// `true` if the innermost frame is a mapping awaiting its key — i.e. a
    /// container arriving now would be a complex key.
    fn key_slot_expecting(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(Frame::Map {
                pending_key: None,
                ..
            })
        )
    }

    /// Insert a completed `node`, recording `anchor` if any and either setting
    /// the root, appending to an array, or pairing it as a key/value entry.
    fn complete(&mut self, node: Node, anchor: usize) {
        if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
        }
        match self.stack.last_mut() {
            None => {
                self.root = Some(node);
            }
            Some(Frame::Array { items, .. }) => items.push(node),
            Some(Frame::Map {
                entries,
                pending_key,
                ..
            }) => {
                if let Some((key, key_span)) = pending_key.take() {
                    entries.push(Entry {
                        key,
                        key_span,
                        value: node,
                    });
                } else {
                    self.set_key(node);
                }
            }
        }
    }

    /// Interpret `node` as a mapping key. Scalars become a [`Key`]; a `<<`
    /// scalar is the unsupported merge key (`E001`); a container (only possible
    /// via an in-scope alias, since live complex keys are caught earlier) is a
    /// complex key (`E001`).
    fn set_key(&mut self, node: Node) {
        let span = node.span();
        let key = match node {
            Node::Null(_) => Key::Null,
            Node::Bool(b, _) => Key::Bool(b),
            Node::Int(i, _) => Key::Int(i),
            Node::Float(f, _) => Key::Float(f),
            Node::String(s, _) => {
                if s == "<<" {
                    self.error = Some(ParseError {
                        span,
                        message: "YAML merge key `<<` is not supported".to_string(),
                    });
                    return;
                }
                Key::String(s)
            }
            Node::Array(..) | Node::Object(..) => {
                self.error = Some(ParseError {
                    span,
                    message: "complex mapping keys are not supported".to_string(),
                });
                return;
            }
        };
        if let Some(Frame::Map { pending_key, .. }) = self.stack.last_mut() {
            *pending_key = Some((key, span));
        }
    }
}

/// Build a leaf [`Node`] from a scalar event. Quoted scalars are always
/// strings; plain scalars go through [`infer_plain`] (tags ignored).
fn make_scalar(v: &str, style: TScalarStyle, span: Span) -> Node {
    if style != TScalarStyle::Plain {
        return Node::String(v.to_string(), span);
    }
    infer_plain(v, span)
}

/// Core-schema inference for a plain scalar, mirroring
/// `yaml-rust2::yaml::Yaml::from_str` but producing a [`Node`].
fn infer_plain(v: &str, span: Span) -> Node {
    if let Some(rest) = v.strip_prefix("0x") {
        if let Ok(i) = i64::from_str_radix(rest, 16) {
            return Node::Int(i, span);
        }
    }
    if let Some(rest) = v.strip_prefix("0o") {
        if let Ok(i) = i64::from_str_radix(rest, 8) {
            return Node::Int(i, span);
        }
    }
    if let Some(rest) = v.strip_prefix('+') {
        if let Ok(i) = rest.parse::<i64>() {
            return Node::Int(i, span);
        }
    }
    match v {
        "" | "~" | "null" => return Node::Null(span),
        "true" | "True" | "TRUE" => return Node::Bool(true, span),
        "false" | "False" | "FALSE" => return Node::Bool(false, span),
        _ => {}
    }
    if let Ok(i) = v.parse::<i64>() {
        return Node::Int(i, span);
    }
    if let Some(f) = parse_f64(v) {
        return Node::Float(f, span);
    }
    Node::String(v.to_string(), span)
}

/// Core-schema float parser matching `yaml-rust2::yaml::parse_f64` so that
/// inference agrees with `yaml-rust2`'s own loader.
fn parse_f64(v: &str) -> Option<f64> {
    match v {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => Some(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => Some(f64::NAN),
        _ if v.as_bytes().iter().any(u8::is_ascii_digit) => v.parse::<f64>().ok(),
        _ => None,
    }
}

/// Drop the span, converting a spanned [`Node`] into the span-less
/// [`crate::ir::Value`] IR. Used for storing raw meta-key (`_ymx` / `_test`)
/// values on the [`Project`](crate::project::Project): downstream crates
/// (`ymx-config` / `ymx-test`) consume the value-level form, not the spanned
/// one. Object keys are stringified per the rule-4 convention (an integer key
/// `0` becomes the string `"0"`); because `Value::Object` is an `IndexMap`, a
/// duplicate stringified key keeps insertion order and the last value wins —
/// matching the value-level view meta consumers need.
pub fn node_to_value(node: &Node) -> Value {
    match node {
        Node::Null(_) => Value::Null,
        Node::Bool(b, _) => Value::Bool(*b),
        Node::Int(i, _) => Value::Int(*i),
        Node::Float(f, _) => Value::Float(*f),
        Node::String(s, _) => Value::String(s.clone()),
        Node::Array(items, _) => Value::Array(items.iter().map(node_to_value).collect()),
        Node::Object(entries, _) => {
            let mut m = indexmap::IndexMap::with_capacity(entries.len());
            for e in entries {
                m.insert(key_to_string(&e.key), node_to_value(&e.value));
            }
            Value::Object(m)
        }
    }
}

/// Stringify a mapping [`Key`] for [`Value::Object`] (rule-4 convention).
fn key_to_string(key: &Key) -> String {
    match key {
        Key::Null => "null".to_string(),
        Key::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Key::Int(i) => i.to_string(),
        Key::Float(f) => crate::ir::render_f64(*f),
        Key::String(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::E001;

    fn parse(src: &str) -> Result<Node, ParseError> {
        parse_document(src)
    }

    fn entry<'a>(node: &'a Node, name: &str) -> &'a Node {
        match node {
            Node::Object(entries, _) => entries
                .iter()
                .find(|e| matches!(&e.key, Key::String(s) if s == name))
                .map(|e| &e.value)
                .unwrap_or_else(|| panic!("no entry {name}")),
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn plain_scalar_types_and_spans() {
        let node = parse("a: 1\nb: 1.5\nc: true\nd: null\ne: hi\nf: \"\"\n").unwrap();
        assert_eq!(entry(&node, "a"), &Node::Int(1, Span { line: 1, col: 4 }));
        assert_eq!(
            entry(&node, "b"),
            &Node::Float(1.5, Span { line: 2, col: 4 })
        );
        assert_eq!(
            entry(&node, "c"),
            &Node::Bool(true, Span { line: 3, col: 4 })
        );
        assert_eq!(entry(&node, "d"), &Node::Null(Span { line: 4, col: 4 }));
        assert_eq!(
            entry(&node, "e"),
            &Node::String("hi".into(), Span { line: 5, col: 4 })
        );
        assert_eq!(
            entry(&node, "f"),
            &Node::String("".into(), Span { line: 6, col: 4 })
        );
    }

    #[test]
    fn key_span_is_one_based() {
        let node = parse("a: 1").unwrap();
        match &node {
            Node::Object(entries, _) => {
                let e = &entries[0];
                assert_eq!(e.key, Key::String("a".into()));
                assert_eq!(e.key_span, Span { line: 1, col: 1 });
            }
            _ => panic!("object"),
        }
    }

    #[test]
    fn quoted_string_is_plain_inference() {
        let node = parse("a: \"1\"\nb: 'true'\n").unwrap();
        assert_eq!(
            entry(&node, "a"),
            &Node::String("1".into(), entry(&node, "a").span())
        );
        assert_eq!(
            entry(&node, "b"),
            &Node::String("true".into(), entry(&node, "b").span())
        );
    }

    #[test]
    fn nested_array_and_object() {
        let node = parse("a:\n  - 1\n  - 2\nb:\n  c: 3\n").unwrap();
        let a = entry(&node, "a");
        assert!(!a.is_scalar());
        match a {
            Node::Array(items, _) => {
                assert_eq!(
                    items,
                    &vec![Node::Int(1, items[0].span()), Node::Int(2, items[1].span()),]
                );
            }
            _ => panic!("array"),
        }
        let b = entry(&node, "b");
        assert_eq!(entry(b, "c"), &Node::Int(3, entry(b, "c").span()));
    }

    #[test]
    fn anchor_and_alias_inlined() {
        let node = parse("a: &x 5\nb: *x\n").unwrap();
        assert_eq!(entry(&node, "a"), &Node::Int(5, entry(&node, "a").span()));
        assert_eq!(entry(&node, "b"), &Node::Int(5, entry(&node, "b").span()));
    }

    #[test]
    fn anchor_to_mapping_inlined() {
        // An alias to a mapping inlines the anchor's value (and its inner spans).
        let node = parse("defaults: &d\n  x: 1\n  y: 2\na: *d\n").unwrap();
        match entry(&node, "a") {
            Node::Object(entries, _) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].key, Key::String("x".into()));
                assert_eq!(entries[0].value, Node::Int(1, entries[0].value.span()));
                assert_eq!(entries[1].key, Key::String("y".into()));
                assert_eq!(entries[1].value, Node::Int(2, entries[1].value.span()));
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn integer_key_distinct_from_string_key() {
        let node = parse("0: a\n\"0\": b\n").unwrap();
        match &node {
            Node::Object(entries, _) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].key, Key::Int(0));
                assert_eq!(entries[1].key, Key::String("0".into()));
            }
            _ => panic!("object"),
        }
    }

    #[test]
    fn explicit_tag_ignored_plain_inference_wins() {
        // `!!str 5` — tag ignored, plain scalar `5` infers to Int.
        let node = parse("a: !!str 5\n").unwrap();
        assert_eq!(entry(&node, "a"), &Node::Int(5, entry(&node, "a").span()));
        // quoted scalar stays a string regardless of tag.
        let node = parse("a: !!int \"5\"\n").unwrap();
        assert_eq!(
            entry(&node, "a"),
            &Node::String("5".into(), entry(&node, "a").span())
        );
    }

    #[test]
    fn empty_string_parses_to_null() {
        let node = parse("").unwrap();
        assert_eq!(node, Node::Null(Span { line: 1, col: 1 }));
    }

    #[test]
    fn document_with_only_separator_parses_to_null() {
        let node = parse("---").unwrap();
        assert_eq!(node, Node::Null(node.span()));
    }

    #[test]
    fn leading_separator_is_single_document() {
        let node = parse("---\na: 1\n").unwrap();
        match &node {
            Node::Object(entries, _) => assert_eq!(entries.len(), 1),
            _ => panic!("object"),
        }
    }

    #[test]
    fn multi_document_stream_is_e001() {
        let err = parse("---\na: 1\n---\nb: 2\n").unwrap_err();
        let diag = err.into_diagnostic(std::path::PathBuf::from("f.yml"));
        assert_eq!(diag.code, E001);
        assert!(diag.message.contains("multi-document"));
        assert_eq!(diag.file, Some(std::path::PathBuf::from("f.yml")));
        // Second `---` is on line 3, column 1.
        assert_eq!((diag.line, diag.col), (3, 1));
    }

    #[test]
    fn merge_key_is_e001() {
        let err = parse("<<: 5\n").unwrap_err();
        let diag = err.into_diagnostic(std::path::PathBuf::from("f.yml"));
        assert_eq!(diag.code, E001);
        assert!(diag.message.contains("merge"));
    }

    #[test]
    fn complex_key_sequence_is_e001() {
        let err = parse("{[a]: b}\n").unwrap_err();
        let diag = err.into_diagnostic(std::path::PathBuf::from("f.yml"));
        assert_eq!(diag.code, E001);
        assert!(diag.message.contains("complex"));
    }

    #[test]
    fn complex_key_mapping_is_e001() {
        let err = parse("? a: b\n: c\n").unwrap_err();
        let diag = err.into_diagnostic(std::path::PathBuf::from("f.yml"));
        assert_eq!(diag.code, E001);
        assert!(diag.message.contains("complex"));
    }

    #[test]
    fn top_level_scalar_recorded_as_scalar() {
        let node = parse("5").unwrap();
        assert!(node.is_scalar());
        assert_eq!(node, Node::Int(5, node.span()));
    }

    #[test]
    fn top_level_array_recorded_as_array() {
        let node = parse("- 1\n- 2\n").unwrap();
        match &node {
            Node::Array(items, _) => assert_eq!(items.len(), 2),
            _ => panic!("array"),
        }
    }

    #[test]
    fn node_to_value_drops_spans_and_preserves_order() {
        let node = parse("a: 1\nb: true\nc:\n  - 1\n  - 2\nd: hi\n").unwrap();
        let v = node_to_value(&node);
        match v {
            Value::Object(m) => {
                assert_eq!(
                    m.keys().collect::<Vec<_>>(),
                    &["a", "b", "c", "d"],
                    "insertion order preserved"
                );
                assert_eq!(m.get("a"), Some(&Value::Int(1)));
                assert_eq!(m.get("b"), Some(&Value::Bool(true)));
                match m.get("c") {
                    Some(Value::Array(items)) => {
                        assert_eq!(items, &vec![Value::Int(1), Value::Int(2)])
                    }
                    _ => panic!("c is array"),
                }
                assert_eq!(m.get("d"), Some(&Value::String("hi".to_string())));
            }
            _ => panic!("object"),
        }
    }

    #[test]
    fn node_to_value_scalar_round_trip() {
        assert_eq!(
            node_to_value(&Node::Null(Span { line: 9, col: 9 })),
            Value::Null
        );
        assert_eq!(
            node_to_value(&Node::Bool(true, Span { line: 9, col: 9 })),
            Value::Bool(true)
        );
        assert_eq!(
            node_to_value(&Node::Int(42, Span { line: 9, col: 9 })),
            Value::Int(42)
        );
        assert_eq!(
            node_to_value(&Node::Float(1.25, Span { line: 9, col: 9 })),
            Value::Float(1.25)
        );
        assert_eq!(
            node_to_value(&Node::String("x".into(), Span { line: 9, col: 9 })),
            Value::String("x".into())
        );
    }

    #[test]
    fn node_to_value_integer_key_stringifies() {
        // Rule-4 convention: integer key 0 -> string "0".
        let node = parse("0: a\n").unwrap();
        match node_to_value(&node) {
            Value::Object(m) => assert_eq!(m.get("0"), Some(&Value::String("a".into()))),
            _ => panic!("object"),
        }
    }
}
