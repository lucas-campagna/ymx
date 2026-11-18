//! Read-only reference extraction over spanned YAML nodes.
//!
//! This is the accessor surface used by static analysis: it walks a
//! [`parse::Node`] tree in source order and records call sites, `from`
//! dispatches, slot declarations, interpolation reads and shell payloads. All
//! string-level work delegates to the existing call-site, interpolation and
//! math parsers; there is no second YAML parser or pattern scanner here.

use crate::callsite::{self, BracePayload, ParsedCall, ParsedValue};
use crate::diag::Span;
use crate::interp::{self, Segment};
use crate::ir::{render_value, Value};
use crate::math::{self, MathRef};
use crate::parse::{key_to_string, Key, Node};
use crate::resolve::{parse_property_key, Suffix};

/// An argument slot. Positional slots are zero-based ordinals: an integer key
/// `n` and a `$n` read both map to `Positional(n - 1)` (with `$0` mapped to 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Named(String),
    Positional(usize),
}

/// A supplied argument at a call site.
#[derive(Debug, Clone, PartialEq)]
pub struct ArgRef {
    pub slot: Slot,
    pub span: Span,
}

/// A reference discovered in a component body.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeRef {
    Call {
        target: String,
        span: Span,
        args: Vec<ArgRef>,
    },
    From {
        target: String,
        span: Span,
    },
    Interpolation {
        slot: Slot,
        span: Span,
    },
    SlotDecl {
        slot: Slot,
        span: Span,
    },
    ShellPayload {
        backend: String,
        span: Span,
        interpolated: bool,
    },
    PropertyKey {
        key: String,
        span: Span,
    },
}

/// Collect all references in `node`, in source order.
///
/// `from_keyword` is the configured `from` keyword (normally `"from"`).
pub fn collect_node_refs(node: &Node, from_keyword: &str) -> Vec<NodeRef> {
    let mut out = Vec::new();
    visit_node(node, from_keyword, true, &mut out);
    out
}

fn visit_node(node: &Node, from_keyword: &str, is_root: bool, out: &mut Vec<NodeRef>) {
    match node {
        Node::String(s, span) => collect_string(s, *span, false, out),
        Node::Array(items, _) => {
            for item in items {
                visit_node(item, from_keyword, false, out);
            }
        }
        Node::Object(entries, _) => {
            for entry in entries {
                visit_key(&entry.key, entry.key_span, &entry.value, is_root, out);
                if let (Key::String(key), Node::String(target, span)) = (&entry.key, &entry.value) {
                    if key == from_keyword && !is_literal_tag(target) {
                        out.push(NodeRef::From {
                            target: target.clone(),
                            span: *span,
                        });
                        if target == "sh" || target == "pw" {
                            let interpolated = entries
                                .iter()
                                .filter(|e| !is_from_key(&e.key, from_keyword))
                                .any(|e| payload_interpolated(&e.value));
                            out.push(NodeRef::ShellPayload {
                                backend: target.clone(),
                                span: *span,
                                interpolated,
                            });
                        }
                    }
                }
                visit_node(&entry.value, from_keyword, false, out);
            }
        }
        _ => {}
    }
}

fn is_from_key(key: &Key, from_keyword: &str) -> bool {
    matches!(key, Key::String(s) if s == from_keyword)
}

fn is_literal_tag(target: &str) -> bool {
    target.len() >= 2 && target.starts_with('<') && target.ends_with('>')
}

fn visit_key(key: &Key, key_span: Span, value: &Node, is_root: bool, out: &mut Vec<NodeRef>) {
    out.push(NodeRef::PropertyKey {
        key: key_to_string(key),
        span: key_span,
    });

    match key {
        Key::Int(i) if *i >= 0 => {
            if is_root {
                out.push(NodeRef::SlotDecl {
                    slot: Slot::Positional((*i as usize).saturating_sub(1)),
                    span: key_span,
                });
            }
        }
        Key::Int(i) => {
            if is_root {
                out.push(NodeRef::SlotDecl {
                    slot: Slot::Named(i.to_string()),
                    span: key_span,
                });
            }
        }
        Key::String(raw) => {
            let Ok(pk) = parse_property_key(raw) else {
                return;
            };
            let slot = slot_from_text(&pk.base);
            if is_root {
                out.push(NodeRef::SlotDecl {
                    slot: slot.clone(),
                    span: key_span,
                });
            }
            match pk.suffix {
                Suffix::None => {}
                Suffix::Math => {
                    if let Node::String(src, span) = value {
                        emit_math_refs(src, *span, out);
                    }
                }
                Suffix::Call(name) => {
                    out.push(NodeRef::Call {
                        target: name.clone(),
                        span: key_span,
                        args: vec![ArgRef {
                            slot,
                            span: value.span(),
                        }],
                    });
                    if name == "sh" || name == "pw" {
                        out.push(NodeRef::ShellPayload {
                            backend: name,
                            span: value.span(),
                            interpolated: payload_interpolated(value),
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_string(src: &str, base: Span, shell: bool, out: &mut Vec<NodeRef>) {
    if !shell {
        if let Ok(Some(call)) = callsite::parse(src) {
            let inner_start = call.name.len() + 2;
            if src.len() > inner_start {
                let args_src = &src[inner_start..src.len() - 1];
                collect_call(&call, args_src, base, out);
                return;
            }
        }
    }

    let segments = if shell {
        interp::scan_shell(src, base)
    } else {
        interp::scan(src, base)
    };
    let Ok(segments) = segments else {
        return;
    };
    for segment in segments {
        match segment {
            Segment::Text(_) => {}
            Segment::Arg { name, span } => out.push(NodeRef::Interpolation {
                slot: slot_from_text(&name),
                span,
            }),
            Segment::Math { src, span } => {
                let math_base = advance(span, "${", 2);
                emit_math_refs(&src, math_base, out);
            }
            Segment::Call { name, args, span } => collect_call_segment(&name, &args, span, out),
            Segment::BraceCall {
                name,
                payload_src,
                span,
            } => collect_brace_call(&name, &payload_src, span, out),
        }
    }
}

fn collect_call_segment(name: &str, args_src: &str, span: Span, out: &mut Vec<NodeRef>) {
    let src = format!("${name}({args_src})");
    if let Ok(Some(call)) = callsite::parse(&src) {
        collect_call(&call, args_src, span, out);
    }
}

fn collect_call(call: &ParsedCall, args_src: &str, base: Span, out: &mut Vec<NodeRef>) {
    let full = format!("${}({args_src})", call.name);
    let inner_start = call.name.len() + 2;
    let mut args = Vec::with_capacity(call.args.len());
    let mut cursor = 0usize;
    let mut positional = 0usize;

    for arg in &call.args {
        let slot = match &arg.key {
            Some(key) => Slot::Named(key.clone()),
            None => {
                let slot = Slot::Positional(positional);
                positional += 1;
                slot
            }
        };
        let (index, len) = find_arg(args_src, cursor, &arg.value);
        args.push(ArgRef {
            slot,
            span: advance(base, &full, inner_start + index),
        });
        cursor = index + len;
    }

    out.push(NodeRef::Call {
        target: call.name.clone(),
        span: base,
        args,
    });

    let args_base = advance(base, &full, inner_start);
    collect_string(args_src, args_base, false, out);
}

fn find_arg(args_src: &str, cursor: usize, value: &ParsedValue) -> (usize, usize) {
    let candidates = arg_tokens(value);
    for token in candidates {
        if let Some(index) = find_from(args_src, cursor, &token) {
            return (index, token.len());
        }
    }
    (cursor.min(args_src.len()), 0)
}

fn arg_tokens(value: &ParsedValue) -> Vec<String> {
    match value {
        ParsedValue::Literal(Value::String(s)) => {
            vec![format!("\"{s}\""), format!("'{s}'"), s.clone()]
        }
        ParsedValue::Literal(v) => match render_value(v) {
            Ok(s) => vec![s],
            Err(_) => vec![match v {
                Value::Array(_) => "[".to_string(),
                _ => "{".to_string(),
            }],
        },
        ParsedValue::Ref { name } => vec![format!("${name}")],
        ParsedValue::Math { src } => vec![format!("${{{src}}}")],
        ParsedValue::Call(call) => vec![format!("${}(", call.name)],
        ParsedValue::Raw(s) => vec![s.clone()],
    }
}

fn find_from(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack.get(from..)?.find(needle).map(|i| from + i)
}

fn collect_brace_call(name: &str, payload_src: &str, span: Span, out: &mut Vec<NodeRef>) {
    let full = format!("${name}{{{payload_src}}}");
    let inner_start = name.len() + 2;
    let payload_span = advance(span, &full, inner_start);

    let mut args = Vec::new();
    let mut cursor = 0usize;
    if let Ok(payload) = callsite::parse_brace_payload(payload_src) {
        match payload {
            BracePayload::Object(entries) => {
                for (key, value) in entries {
                    let (index, len) = find_literal(payload_src, cursor, &value);
                    args.push(ArgRef {
                        slot: slot_from_text(&key),
                        span: advance(span, &full, inner_start + index),
                    });
                    cursor = index + len;
                }
            }
            BracePayload::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    let (found, len) = find_literal(payload_src, cursor, value);
                    args.push(ArgRef {
                        slot: Slot::Positional(index),
                        span: advance(span, &full, inner_start + found),
                    });
                    cursor = found + len;
                }
            }
            BracePayload::Scalar(value) => {
                let (found, _) = find_literal(payload_src, 0, &value);
                args.push(ArgRef {
                    slot: Slot::Positional(0),
                    span: advance(span, &full, inner_start + found),
                });
            }
        }
    }

    out.push(NodeRef::Call {
        target: name.to_string(),
        span,
        args,
    });

    let is_shell = name == "sh" || name == "pw";
    if is_shell {
        out.push(NodeRef::ShellPayload {
            backend: name.to_string(),
            span: payload_span,
            interpolated: string_has_interpolation(payload_src, payload_span),
        });
    }
    collect_string(payload_src, payload_span, is_shell, out);
}

fn find_literal(payload_src: &str, cursor: usize, value: &Value) -> (usize, usize) {
    let tokens = match value {
        Value::String(s) => vec![format!("\"{s}\""), format!("'{s}'"), s.clone()],
        _ => match render_value(value) {
            Ok(s) => vec![s],
            Err(_) => vec![match value {
                Value::Array(_) => "[".to_string(),
                _ => "{".to_string(),
            }],
        },
    };
    for token in tokens {
        if let Some(index) = find_from(payload_src, cursor, &token) {
            return (index, token.len());
        }
    }
    (cursor.min(payload_src.len()), 0)
}

fn emit_math_refs(src: &str, base: Span, out: &mut Vec<NodeRef>) {
    let Ok(refs) = math::scan_refs(src) else {
        return;
    };
    for reference in refs {
        emit_math_ref(&reference, src, base, out);
    }
}

fn emit_math_ref(reference: &MathRef, src: &str, base: Span, out: &mut Vec<NodeRef>) {
    match reference {
        MathRef::Arg { name, offset } => out.push(NodeRef::Interpolation {
            slot: slot_from_text(name),
            span: advance(base, src, *offset),
        }),
        MathRef::Pos { index, offset } => out.push(NodeRef::Interpolation {
            slot: Slot::Positional(index.saturating_sub(1)),
            span: advance(base, src, *offset),
        }),
        MathRef::Call { name, offset, args } => {
            let mut positional = 0usize;
            let arg_refs = args
                .iter()
                .map(|arg| {
                    let slot = match arg {
                        MathRef::Arg { name, .. } => slot_from_text(name),
                        MathRef::Pos { index, .. } => Slot::Positional(index.saturating_sub(1)),
                        MathRef::Call { .. } => {
                            let slot = Slot::Positional(positional);
                            positional += 1;
                            slot
                        }
                    };
                    ArgRef {
                        slot,
                        span: math_ref_span(arg, src, base),
                    }
                })
                .collect();
            out.push(NodeRef::Call {
                target: name.clone(),
                span: advance(base, src, *offset),
                args: arg_refs,
            });
            for arg in args {
                emit_math_ref(arg, src, base, out);
            }
        }
    }
}

fn math_ref_span(reference: &MathRef, src: &str, base: Span) -> Span {
    let offset = match reference {
        MathRef::Arg { offset, .. }
        | MathRef::Pos { offset, .. }
        | MathRef::Call { offset, .. } => *offset,
    };
    advance(base, src, offset)
}

fn slot_from_text(text: &str) -> Slot {
    match text.parse::<usize>() {
        Ok(n) => Slot::Positional(n.saturating_sub(1)),
        Err(_) => Slot::Named(text.to_string()),
    }
}

fn payload_interpolated(node: &Node) -> bool {
    match node {
        Node::String(s, span) => string_has_interpolation(s, *span),
        Node::Array(items, _) => items.iter().any(payload_interpolated),
        Node::Object(entries, _) => entries.iter().any(|e| payload_interpolated(&e.value)),
        _ => false,
    }
}

fn string_has_interpolation(src: &str, base: Span) -> bool {
    match interp::scan_shell(src, base) {
        Ok(segments) => segments
            .iter()
            .any(|segment| !matches!(segment, Segment::Text(_))),
        Err(_) => false,
    }
}

fn advance(base: Span, text: &str, bytes: usize) -> Span {
    let prefix = &text[..bytes.min(text.len())];
    let newlines = prefix.matches('\n').count() as u32;
    if newlines == 0 {
        Span {
            line: base.line,
            col: base.col + prefix.chars().count() as u32,
        }
    } else {
        let tail = prefix.rsplit('\n').next().unwrap_or("");
        Span {
            line: base.line + newlines,
            col: 1 + tail.chars().count() as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_document;

    fn refs(src: &str) -> Vec<NodeRef> {
        let node = parse_document(src).unwrap();
        collect_node_refs(&node, "from")
    }

    fn body_refs(src: &str) -> Vec<NodeRef> {
        let node = parse_document(src).unwrap();
        let Node::Object(entries, _) = &node else {
            panic!("not an object");
        };
        let body = entries
            .iter()
            .find(|e| matches!(&e.key, Key::String(s) if s == "main"))
            .map(|e| &e.value)
            .expect("no main component");
        collect_node_refs(body, "from")
    }

    fn span(line: u32, col: u32) -> Span {
        Span { line, col }
    }

    fn call(target: &str, line: u32, col: u32, args: Vec<ArgRef>) -> NodeRef {
        NodeRef::Call {
            target: target.to_string(),
            span: span(line, col),
            args,
        }
    }

    fn arg(slot: Slot, line: u32, col: u32) -> ArgRef {
        ArgRef {
            slot,
            span: span(line, col),
        }
    }

    fn interpolation(slot: Slot, line: u32, col: u32) -> NodeRef {
        NodeRef::Interpolation {
            slot,
            span: span(line, col),
        }
    }

    fn slot_decl(slot: Slot, line: u32, col: u32) -> NodeRef {
        NodeRef::SlotDecl {
            slot,
            span: span(line, col),
        }
    }

    fn key(key: &str, line: u32, col: u32) -> NodeRef {
        NodeRef::PropertyKey {
            key: key.to_string(),
            span: span(line, col),
        }
    }

    #[test]
    fn whole_string_call_with_nested_args() {
        let out = body_refs("main: \"$box($x, 2, $inner($y))\"\n");
        assert_eq!(
            out[0],
            call(
                "box",
                1,
                7,
                vec![
                    arg(Slot::Positional(0), 1, 12),
                    arg(Slot::Positional(1), 1, 16),
                    arg(Slot::Positional(2), 1, 19),
                ]
            )
        );
        assert_eq!(out[1], interpolation(Slot::Named("x".into()), 1, 12));
        assert_eq!(
            out[2],
            call("inner", 1, 19, vec![arg(Slot::Positional(0), 1, 26)])
        );
        assert_eq!(out[3], interpolation(Slot::Named("y".into()), 1, 26));
    }

    #[test]
    fn from_and_math_interpolation() {
        let out = body_refs("main:\n  from: detail\n  value: hi $name ${x + f(1)}\n");
        assert!(out.contains(&NodeRef::From {
            target: "detail".into(),
            span: span(2, 9),
        }));
        assert!(out.contains(&interpolation(Slot::Named("name".into()), 3, 13)));
        assert!(out.contains(&interpolation(Slot::Named("x".into()), 3, 21)));
        assert!(out.contains(&call("f", 3, 25, vec![])));
    }

    #[test]
    fn slot_declarations_and_modifiers() {
        let out =
            body_refs("main:\n  0: first\n  x?: second\n  y$: 1 + 2\n  main$sh: echo $name\n");
        assert!(out.contains(&slot_decl(Slot::Positional(0), 2, 3)));
        assert!(out.contains(&slot_decl(Slot::Named("x".into()), 3, 3)));
        assert!(out.contains(&slot_decl(Slot::Named("y".into()), 4, 3)));
        assert!(out.contains(&slot_decl(Slot::Named("main".into()), 5, 3)));
        assert!(out.contains(&key("main$sh", 5, 3)));
        assert!(out.contains(&call(
            "sh",
            5,
            3,
            vec![arg(Slot::Named("main".into()), 5, 12)]
        )));
        assert!(out.contains(&NodeRef::ShellPayload {
            backend: "sh".into(),
            span: span(5, 12),
            interpolated: true,
        }));
    }

    #[test]
    fn slot_decls_only_for_root_entries() {
        let out = body_refs("main:\n  x: 1\n  box:\n    y: 2\n");
        assert_eq!(
            out.iter()
                .filter(|r| matches!(r, NodeRef::SlotDecl { .. }))
                .collect::<Vec<_>>(),
            vec![
                &slot_decl(Slot::Named("x".into()), 2, 3),
                &slot_decl(Slot::Named("box".into()), 3, 3),
            ]
        );
        assert!(out.contains(&key("y", 4, 5)));
        assert!(!out.contains(&slot_decl(Slot::Named("y".into()), 4, 5)));
    }

    #[test]
    fn escaped_dollar_is_not_a_shell_interpolation() {
        let out = body_refs("main:\n  x$sh: echo \\$name\n");
        assert!(out.contains(&NodeRef::ShellPayload {
            backend: "sh".into(),
            span: span(2, 9),
            interpolated: false,
        }));
        assert!(out
            .iter()
            .all(|r| !matches!(r, NodeRef::Interpolation { .. })));
    }

    #[test]
    fn brace_shell_and_from_shell() {
        let out = refs("main: $sh{echo $name}\nother:\n  from: pw\n  cmd: hi $x\n");
        assert!(out.contains(&call("sh", 1, 7, vec![arg(Slot::Positional(0), 1, 11)])));
        assert!(out.contains(&NodeRef::ShellPayload {
            backend: "sh".into(),
            span: span(1, 11),
            interpolated: true,
        }));
        assert!(out.contains(&NodeRef::From {
            target: "pw".into(),
            span: span(3, 9),
        }));
        assert!(out.contains(&NodeRef::ShellPayload {
            backend: "pw".into(),
            span: span(3, 9),
            interpolated: true,
        }));
    }

    #[test]
    fn literal_tag_from_is_skipped() {
        let out = refs("main:\n  from: <div>\n");
        assert!(out.iter().all(|r| !matches!(r, NodeRef::From { .. })));
        assert!(out.contains(&key("from", 2, 3)));
    }
}
