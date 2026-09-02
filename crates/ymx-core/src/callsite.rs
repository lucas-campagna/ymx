//! Rule-3 inline call-site parsing: whole-string `$name(...)` values.
//!
//! A YAML string scalar is an inline call-site exactly when the **entire**
//! string matches the shape `$name(...)` — a `$`, an effective identifier
//! (`[A-Za-z_][A-Za-z0-9_]*`), an opening `(`, a balanced argument list
//! (parens, `${...}` braces, and `'`/`"` quotes tracked), and a final `)`
//! with nothing after it. Anything else (leading text, trailing text, no
//! parens) is *not* a call-site and falls back to ordinary string
//! interpolation (PRD rule 3, *Argument value parsing*).
//!
//! The argument list is comma-separated; each argument is positional `value`
//! or named `key=value` (the key is an effective identifier). A positional
//! argument after a named one is `E012`; `()` calls with no arguments.
//! Argument values parse in order as Null (`null`/`~`), Bool
//! (`true`/`false`), Int, Float (decimal or exponent), then String; quoted
//! tokens are always Strings (`"..."` processes the shared `\$` / `\\`
//! escapes, `'...'` is verbatim); `$name` / `$N` references, `${...}`, and
//! nested `$name(...)` calls are resolved as nested call-sites (rule 11); a
//! direct array/object literal argument (`[...]` / `{...}`) is `E013`.
//!
//! The parser is pure — it carries no scope — and reports errors as
//! `(code, message)` pairs the resolver attributes to the call-site's
//! diagnostic context. Shape mismatches yield `Ok(None)` so the caller can
//! fall back to interpolation.

use crate::ir::Value;
use indexmap::IndexMap;

/// A parsed inline call-site `$name(...)` (rule 3).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCall {
    /// The call target's single effective identifier (namespace-local).
    pub name: String,
    /// The argument list, in written order.
    pub args: Vec<ParsedArg>,
}

/// One parsed call-site argument.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedArg {
    /// `Some(key)` for a named `key=value` argument, `None` for positional.
    pub key: Option<String>,
    /// The argument's parsed value.
    pub value: ParsedValue,
}

/// A parsed argument value token.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedValue {
    /// A resolved literal: Null, Bool, Int, Float, or a quoted String.
    Literal(Value),
    /// A bare `$name` / `$N` reference — interpolated in the caller's scope
    /// (rule-2 fallback applies).
    Ref { name: String },
    /// A `${...}` expression — evaluated in the caller's scope.
    Math { src: String },
    /// A nested `$name(...)` call-site.
    Call(Box<ParsedCall>),
    /// Any other unquoted text: a plain String value.
    Raw(String),
}

/// The parsed payload of a `$name{...}` brace call (rule 22).
#[derive(Debug, Clone, PartialEq)]
pub enum BracePayload {
    /// An object literal `{k: v, ...}`: each entry becomes a named argument.
    /// Keys are identifier strings only (E010 for non-identifier keys).
    Object(Vec<(String, Value)>),
    /// An array literal `[v0, v1, ...]`: each entry becomes a positional argument ($0, $1, ...).
    Array(Vec<Value>),
    /// A scalar value: a single argument (scalar → $0 in the binding table).
    Scalar(Value),
}

/// Parse `src` as an inline call-site.
///
/// `Ok(None)` means the string is not a call-site (fall back to string
/// interpolation). `Err((code, message))` means it *looks* like a call-site
/// but violates the grammar: `E010` malformed syntax (unterminated parens,
/// bad quoted token, invalid argument name, empty argument, bad escapes),
/// `E012` positional-after-named, or `E013` array/object literal argument.
pub fn parse(src: &str) -> Result<Option<ParsedCall>, (&'static str, String)> {
    match parse_prefix(src)? {
        Some((call, consumed)) if consumed == src.len() => Ok(Some(call)),
        _ => Ok(None),
    }
}

/// Parse a call-site prefix from `src`.
///
/// Returns the parsed call plus the number of bytes consumed when `src`
/// begins with a `$name(...)` shape; `Ok(None)` means `src` does not start
/// with a call-site.
pub(crate) fn parse_prefix(
    src: &str,
) -> Result<Option<(ParsedCall, usize)>, (&'static str, String)> {
    let b = src.as_bytes();
    let mut i = 0;
    if b.get(i) != Some(&b'$') {
        return Ok(None);
    }
    i += 1;
    let name_start = i;
    if !is_ident_start(b.get(i).copied()) {
        return Ok(None);
    }
    while is_ident_cont(b.get(i).copied()) {
        i += 1;
    }
    let name = &src[name_start..i];
    if b.get(i) != Some(&b'(') {
        return Ok(None);
    }
    let open = i;
    let Some(close) = matching_paren(b, open) else {
        return Err((
            crate::diag::E010,
            format!("unterminated call-site `{src}` (missing `)`)"),
        ));
    };
    let args = parse_args(&src[open + 1..close])?;
    Ok(Some((
        ParsedCall {
            name: name.to_string(),
            args,
        },
        close + 1,
    )))
}

/// The index of the `)` matching the `(` at `open`, tracking nested parens,
/// `${...}` blocks (opaque — math has no `}` and its parens are balanced),
/// and `'`/`"` quotes (with backslash escapes); `None` if unbalanced.
fn matching_paren(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut i = open + 1;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    let mut paren = 0usize;
                    let mut j = i + 2;
                    loop {
                        if j >= b.len() {
                            return None;
                        }
                        match b[j] {
                            b'(' => paren += 1,
                            b')' if paren == 0 => return None,
                            b')' => paren -= 1,
                            b'}' if paren == 0 => {
                                i = j;
                                break;
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                }
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Parse the argument list between the call-site parens (`()` = no args).
fn parse_args(inner: &str) -> Result<Vec<ParsedArg>, (&'static str, String)> {
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let chunks = split_top_level(inner)?;
    let mut args = Vec::with_capacity(chunks.len());
    let mut saw_named = false;
    for chunk in chunks {
        let arg = parse_arg(chunk)?;
        if arg.key.is_none() && saw_named {
            return Err((
                crate::diag::E012,
                "positional argument after named argument".to_string(),
            ));
        }
        if arg.key.is_some() {
            saw_named = true;
        }
        args.push(arg);
    }
    Ok(args)
}

/// Split `s` on top-level commas (quotes, parens, and `${...}` blocks are
/// opaque). Unterminated quotes/parens are `E010`.
fn split_top_level(s: &str) -> Result<Vec<&str>, (&'static str, String)> {
    let b = s.as_bytes();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut quote: Option<u8> = None;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    let mut inner_paren = 0usize;
                    let mut j = i + 2;
                    loop {
                        if j >= b.len() {
                            return Err((
                                crate::diag::E010,
                                format!("unterminated `${{...}}` in call-site argument `{s}`"),
                            ));
                        }
                        match b[j] {
                            b'(' => inner_paren += 1,
                            b')' if inner_paren == 0 => {
                                return Err((
                                    crate::diag::E010,
                                    format!("malformed `${{...}}` in call-site argument `{s}`"),
                                ))
                            }
                            b')' => inner_paren -= 1,
                            b'}' if inner_paren == 0 => {
                                i = j;
                                break;
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                }
                b'(' => paren += 1,
                b')' => {
                    if paren == 0 {
                        return Err((
                            crate::diag::E010,
                            format!("unbalanced `)` in call-site argument `{s}`"),
                        ));
                    }
                    paren -= 1;
                }
                b'[' => bracket += 1,
                b']' => {
                    if bracket == 0 {
                        return Err((
                            crate::diag::E010,
                            format!("unbalanced `]` in call-site argument `{s}`"),
                        ));
                    }
                    bracket -= 1;
                }
                b'{' => brace += 1,
                b'}' => {
                    if brace == 0 {
                        return Err((
                            crate::diag::E010,
                            format!("unbalanced `}}` in call-site argument `{s}`"),
                        ));
                    }
                    brace -= 1;
                }
                b',' if paren == 0 && bracket == 0 && brace == 0 => {
                    chunks.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return Err((
            crate::diag::E010,
            format!("unterminated quoted string in call-site argument `{s}`"),
        ));
    }
    chunks.push(&s[start..]);
    Ok(chunks)
}

/// Parse one argument chunk: `key=value` (key = effective identifier) or a
/// bare value.
fn parse_arg(chunk: &str) -> Result<ParsedArg, (&'static str, String)> {
    match top_level_eq(chunk) {
        Some(eq) => {
            let key = chunk[..eq].trim();
            if !is_identifier(key) {
                return Err((
                    crate::diag::E010,
                    format!("invalid argument name `{key}` (expected an effective identifier)"),
                ));
            }
            let value = parse_value(chunk[eq + 1..].trim())?;
            Ok(ParsedArg {
                key: Some(key.to_string()),
                value,
            })
        }
        None => {
            let value = parse_value(chunk.trim())?;
            Ok(ParsedArg { key: None, value })
        }
    }
}

/// The byte index of the first `=` at paren/quote depth zero (a `=` inside a
/// quoted token or `${...}` does not split), if any.
fn top_level_eq(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut quote: Option<u8> = None;
    let mut paren = 0usize;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    let mut inner_paren = 0usize;
                    let mut j = i + 2;
                    while j < b.len() {
                        match b[j] {
                            b'(' => inner_paren += 1,
                            b')' if inner_paren > 0 => inner_paren -= 1,
                            b'}' if inner_paren == 0 => break,
                            _ => {}
                        }
                        j += 1;
                    }
                    i = j;
                }
                b'(' => paren += 1,
                b')' if paren > 0 => paren -= 1,
                b'=' if paren == 0 => return Some(i),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Parse one argument value token per PRD *Argument value parsing*.
fn parse_value(text: &str) -> Result<ParsedValue, (&'static str, String)> {
    let b = text.as_bytes();
    match b.first().copied() {
        None => Err((crate::diag::E010, "empty argument".to_string())),
        Some(q @ (b'\'' | b'"')) => match closing_quote(b, q) {
            Some(c) if c == b.len() - 1 => {
                let inner = &text[1..c];
                let s = if q == b'"' {
                    unescape(inner)?
                } else {
                    inner.to_string()
                };
                Ok(ParsedValue::Literal(Value::string(s)))
            }
            _ => Err((
                crate::diag::E010,
                format!("malformed quoted argument `{text}`"),
            )),
        },
        Some(b'$') => parse_dollar(text),
        Some(b'[') | Some(b'{') => parse_yaml_literal(text),
        Some(_) => {
            if text == "null" || text == "~" {
                Ok(ParsedValue::Literal(Value::null()))
            } else if text == "true" {
                Ok(ParsedValue::Literal(Value::bool(true)))
            } else if text == "false" {
                Ok(ParsedValue::Literal(Value::bool(false)))
            } else if let Ok(i) = text.parse::<i64>() {
                Ok(ParsedValue::Literal(Value::int(i)))
            } else if is_numeric_start(b[0]) && text.parse::<f64>().is_ok() {
                Ok(ParsedValue::Literal(Value::float(
                    text.parse::<f64>().unwrap(),
                )))
            } else {
                Ok(ParsedValue::Raw(text.to_string()))
            }
        }
    }
}

/// Parse a `$`-prefixed argument value: `$name` / `$N` reference, `${...}`
/// math, or a nested `$name(...)` call-site; anything else is plain String
/// text (no interpolation inside call-site argument tokens).
fn parse_dollar(text: &str) -> Result<ParsedValue, (&'static str, String)> {
    let rest = &text[1..];
    let b = rest.as_bytes();
    if rest.starts_with('{') {
        return match rest.find('}') {
            Some(c) if c == rest.len() - 1 => Ok(ParsedValue::Math {
                src: rest[1..c].to_string(),
            }),
            _ => Err((
                crate::diag::E010,
                format!("malformed math argument `{text}`"),
            )),
        };
    }
    if is_ident_start(b.first().copied()) {
        let mut i = 0;
        while is_ident_cont(b.get(i).copied()) {
            i += 1;
        }
        let name = &rest[..i];
        let after = &rest[i..];
        if after.is_empty() {
            return Ok(ParsedValue::Ref {
                name: name.to_string(),
            });
        }
        if after.starts_with('(') {
            return match parse(text)? {
                Some(call) => Ok(ParsedValue::Call(Box::new(call))),
                None => Err((
                    crate::diag::E010,
                    format!("malformed nested call-site `{text}`"),
                )),
            };
        }
    } else if !b.is_empty() && b.iter().all(|c| c.is_ascii_digit()) {
        return Ok(ParsedValue::Ref {
            name: rest.to_string(),
        });
    }
    Ok(ParsedValue::Raw(text.to_string()))
}

/// Parse an inline YAML array or object literal (`[...]` or `{...}`) into a
/// [`ParsedValue::Literal`]. Handles compact flow syntax (`{x:1}`) that
/// `yaml_rust2` treats as a scalar.
fn parse_yaml_literal(text: &str) -> Result<ParsedValue, (&'static str, String)> {
    let text = text.trim();
    if text.starts_with('[') {
        let inner = &text[1..text.len() - 1].trim();
        if inner.is_empty() {
            return Ok(ParsedValue::Literal(Value::array(vec![])));
        }
        let items = split_flow_items(inner)?;
        let values: Result<Vec<Value>, _> = items.iter().map(|s| parse_flow_value(s)).collect();
        Ok(ParsedValue::Literal(Value::array(values?)))
    } else if text.starts_with('{') {
        let inner = &text[1..text.len() - 1].trim();
        if inner.is_empty() {
            return Ok(ParsedValue::Literal(Value::Object(IndexMap::new())));
        }
        let pairs = split_flow_items(inner)?;
        let mut map = IndexMap::new();
        for pair in pairs {
            let (k, v) = parse_flow_kv(&pair)?;
            map.insert(k, v);
        }
        Ok(ParsedValue::Literal(Value::Object(map)))
    } else {
        Err((
            crate::diag::E010,
            format!("expected `[...]` or `{{...}}`, got `{text}`"),
        ))
    }
}

/// Split a flow-level comma-separated list, respecting nested `[]`, `{}`,
/// quotes, and `${...}`.
fn split_flow_items(s: &str) -> Result<Vec<String>, (&'static str, String)> {
    let b = s.as_bytes();
    let mut items = Vec::new();
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut quote: Option<u8> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    let mut inner_paren = 0i32;
                    let mut j = i + 2;
                    while j < b.len() {
                        match b[j] {
                            b'(' => inner_paren += 1,
                            b')' if inner_paren > 0 => inner_paren -= 1,
                            b'}' if inner_paren == 0 => break,
                            _ => {}
                        }
                        j += 1;
                    }
                    i = j;
                }
                b'(' => depth_paren += 1,
                b')' => depth_paren -= 1,
                b'[' => depth_bracket += 1,
                b']' => depth_bracket -= 1,
                b'{' => depth_brace += 1,
                b'}' => depth_brace -= 1,
                b',' if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 => {
                    items.push(s[start..i].trim().to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    items.push(s[start..].trim().to_string());
    Ok(items)
}

/// Parse a single flow value (scalar, nested array, nested object, or math).
fn parse_flow_value(text: &str) -> Result<Value, (&'static str, String)> {
    let text = text.trim();
    if text.starts_with('[') {
        let inner = &text[1..text.len() - 1].trim();
        if inner.is_empty() {
            return Ok(Value::array(vec![]));
        }
        let items = split_flow_items(inner)?;
        let values: Result<Vec<Value>, _> = items.iter().map(|s| parse_flow_value(s)).collect();
        Ok(Value::array(values?))
    } else if text.starts_with('{') {
        let inner = &text[1..text.len() - 1].trim();
        if inner.is_empty() {
            return Ok(Value::Object(IndexMap::new()));
        }
        let pairs = split_flow_items(inner)?;
        let mut map = IndexMap::new();
        for pair in pairs {
            let (k, v) = parse_flow_kv(&pair)?;
            map.insert(k, v);
        }
        Ok(Value::Object(map))
    } else if text.starts_with('"') || text.starts_with('\'') {
        parse_flow_quoted(text)
    } else if text == "null" || text == "~" {
        Ok(Value::null())
    } else if text == "true" {
        Ok(Value::bool(true))
    } else if text == "false" {
        Ok(Value::bool(false))
    } else if let Ok(i) = text.parse::<i64>() {
        Ok(Value::int(i))
    } else if let Ok(f) = text.parse::<f64>() {
        Ok(Value::float(f))
    } else if text.starts_with("${") && text.ends_with('}') {
        Err((
            crate::diag::E010,
            "math expressions are not supported in flow literals".to_string(),
        ))
    } else {
        Ok(Value::string(text.to_string()))
    }
}

/// Parse a `key: value` pair from a flow object.
fn parse_flow_kv(text: &str) -> Result<(String, Value), (&'static str, String)> {
    let text = text.trim();
    let colon = text.find(':').ok_or((
        crate::diag::E010,
        format!("expected `key: value` in object literal, got `{text}`"),
    ))?;
    let key = text[..colon].trim().to_string();
    let val_text = text[colon + 1..].trim();
    let value = parse_flow_value(val_text)?;
    Ok((key, value))
}

/// Parse a quoted string literal (single or double quoted).
fn parse_flow_quoted(text: &str) -> Result<Value, (&'static str, String)> {
    let q = text.as_bytes()[0];
    if text.len() < 2 || text.as_bytes()[text.len() - 1] != q {
        return Err((
            crate::diag::E010,
            format!("unterminated quoted string `{text}`"),
        ));
    }
    let inner = &text[1..text.len() - 1];
    if q == b'"' {
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => {
                        return Err((
                            crate::diag::E010,
                            "trailing backslash in string".to_string(),
                        ));
                    }
                }
            } else {
                out.push(c);
            }
        }
        Ok(Value::string(out))
    } else {
        Ok(Value::string(inner.to_string()))
    }
}

/// The index of the closing quote for a quoted token starting at index 0
/// with quote byte `q`, honoring backslash escapes in double quotes;
/// `None` if unterminated.
fn closing_quote(b: &[u8], q: u8) -> Option<usize> {
    let mut i = 1;
    while i < b.len() {
        if q == b'"' && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Process the shared string escapes inside a double-quoted token:
/// `\$` → `$`, `\\` → `\`; any other `\X` is `E010`.
fn unescape(inner: &str) -> Result<String, (&'static str, String)> {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('$') => out.push('$'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                return Err((
                    crate::diag::E010,
                    format!("invalid escape `\\{other}` in call-site argument (only `\\$` and `\\\\` are allowed)"),
                ));
            }
            None => {
                return Err((
                    crate::diag::E010,
                    "trailing escape `\\` in call-site argument (only `\\$` and `\\\\` are allowed)"
                        .to_string(),
                ));
            }
        }
    }
    Ok(out)
}

fn is_ident_start(c: Option<u8>) -> bool {
    matches!(c, Some(c) if c.is_ascii_alphabetic() || c == b'_')
}

fn is_ident_cont(c: Option<u8>) -> bool {
    matches!(c, Some(c) if c.is_ascii_alphanumeric() || c == b'_')
}

/// An effective identifier: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A Float token must look like a number (reject `inf` / `NaN`, which
/// `f64::from_str` would accept but YMX treats as String text).
fn is_numeric_start(c: u8) -> bool {
    c.is_ascii_digit() || c == b'-' || c == b'.' || c == b'+'
}

/// Parse the raw payload of a `$name{...}` brace call.
///
/// Returns the parsed payload as one of:
/// - `BracePayload::Object` — `{k: v, ...}` → named args (keys must be identifiers, else E010)
/// - `BracePayload::Array` — `[v0, v1, ...]` → positional args
/// - `BracePayload::Scalar` — a single scalar value → scalar arg
///
/// E010 for malformed payloads: non-identifier object key, unbalanced braces/brackets,
/// unterminated quotes.
pub fn parse_brace_payload(src: &str) -> Result<BracePayload, (&'static str, String)> {
    let src = src.trim();
    if src.is_empty() {
        // Empty payload → no args (handled as Args::None in the resolver)
        return Ok(BracePayload::Object(Vec::new()));
    }
    let b = src.as_bytes();
    match b.first() {
        Some(&b'{') => parse_brace_object(src),
        Some(&b'[') => parse_brace_array(src),
        _ => parse_brace_scalar(src),
    }
}

/// Parse a brace-call object payload `{k: v, ...}`.
fn parse_brace_object(src: &str) -> Result<BracePayload, (&'static str, String)> {
    let inner = &src[1..src.len() - 1].trim();
    if inner.is_empty() {
        return Ok(BracePayload::Object(Vec::new()));
    }
    let pairs = split_flow_items(inner)?;
    let mut entries = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let (k, v) = parse_brace_kv(&pair)?;
        // Object keys in brace-call payloads must be identifiers (else E010).
        if !is_identifier(&k) {
            return Err((
                crate::diag::E010,
                format!(
                    "invalid key `{k}` in brace-call object payload (expected an effective identifier)"
                ),
            ));
        }
        entries.push((k, v));
    }
    Ok(BracePayload::Object(entries))
}

/// Parse a `key: value` pair in a brace-call object payload.
fn parse_brace_kv(text: &str) -> Result<(String, Value), (&'static str, String)> {
    let text = text.trim();
    let colon = text.find(':').ok_or((
        crate::diag::E010,
        format!("expected `key: value` in brace-call object payload, got `{text}`"),
    ))?;
    let key = text[..colon].trim().to_string();
    let val_text = text[colon + 1..].trim();
    let value = parse_flow_value(val_text)?;
    Ok((key, value))
}

/// Parse a brace-call array payload `[v0, v1, ...]`.
fn parse_brace_array(src: &str) -> Result<BracePayload, (&'static str, String)> {
    let inner = &src[1..src.len() - 1].trim();
    if inner.is_empty() {
        return Ok(BracePayload::Array(Vec::new()));
    }
    let items = split_flow_items(inner)?;
    let values: Result<Vec<Value>, _> = items.iter().map(|s| parse_flow_value(s)).collect();
    Ok(BracePayload::Array(values?))
}

/// Parse a brace-call scalar payload.
fn parse_brace_scalar(src: &str) -> Result<BracePayload, (&'static str, String)> {
    // Reuse the existing parse_value to handle null/bool/int/float/string/quoted.
    let value = match parse_value(src) {
        Ok(ParsedValue::Literal(v)) => v,
        Ok(ParsedValue::Raw(s)) => Value::String(s),
        Ok(ParsedValue::Ref { name }) => {
            // A bare `$name` in a scalar brace-call payload is stored as a string reference.
            Value::String(format!("${}", name))
        }
        Ok(ParsedValue::Math { src: m }) => Value::String(format!("${{{}}}", m)),
        Ok(ParsedValue::Call(_)) => {
            return Err((
                crate::diag::E010,
                "nested call-site `$name(...)` not supported in scalar brace-call payload"
                    .to_string(),
            ));
        }
        Err((code, msg)) => return Err((code, msg)),
    };
    Ok(BracePayload::Scalar(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(value: ParsedValue) -> ParsedArg {
        ParsedArg { key: None, value }
    }

    fn named(key: &str, value: ParsedValue) -> ParsedArg {
        ParsedArg {
            key: Some(key.to_string()),
            value,
        }
    }

    fn lit(v: Value) -> ParsedValue {
        ParsedValue::Literal(v)
    }

    fn call(name: &str, args: Vec<ParsedArg>) -> ParsedValue {
        ParsedValue::Call(Box::new(ParsedCall {
            name: name.to_string(),
            args,
        }))
    }

    #[test]
    fn shape_detection() {
        assert!(parse("$x()").unwrap().is_some());
        assert!(parse("$x(1, 2)").unwrap().is_some());
        for not_a_call in [
            "abc", "$x", "$x ", "$x (1)", "x(1)", "$1(2)", "$$x(1)", "cost $5", "$x(1)!",
            "$x(1)(2)", "a$b(1)",
        ] {
            assert_eq!(parse(not_a_call).unwrap(), None, "{not_a_call}");
        }
    }

    #[test]
    fn empty_and_mixed_argument_lists() {
        assert_eq!(parse("$x()").unwrap().unwrap().args, vec![]);
        let call = parse("$x(1, 2)").unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![pos(lit(Value::int(1))), pos(lit(Value::int(2)))]
        );
        let call = parse("$x(a=1, b=2)").unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![
                named("a", lit(Value::int(1))),
                named("b", lit(Value::int(2)))
            ]
        );
        let call = parse("$x(1, a=2)").unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![pos(lit(Value::int(1))), named("a", lit(Value::int(2)))]
        );
    }

    #[test]
    fn positional_after_named_is_e012() {
        let (code, _) = parse("$x(a=1, 2)").unwrap_err();
        assert_eq!(code, crate::diag::E012);
    }

    #[test]
    fn literal_values() {
        let call = parse("$x(null, ~, true, false, 12, -3, 1.5, 1e3, abc, \"s\", 't', 0x10, inf)")
            .unwrap()
            .unwrap();
        let expect = vec![
            pos(lit(Value::null())),
            pos(lit(Value::null())),
            pos(lit(Value::bool(true))),
            pos(lit(Value::bool(false))),
            pos(lit(Value::int(12))),
            pos(lit(Value::int(-3))),
            pos(lit(Value::float(1.5))),
            pos(lit(Value::float(1000.0))),
            pos(ParsedValue::Raw("abc".to_string())),
            pos(lit(Value::string("s"))),
            pos(lit(Value::string("t"))),
            pos(ParsedValue::Raw("0x10".to_string())),
            pos(ParsedValue::Raw("inf".to_string())),
        ];
        assert_eq!(call.args, expect);
    }

    #[test]
    fn quoted_tokens_are_strings() {
        assert_eq!(
            parse(r#"$x("a\$b", 'c\d', "a b")"#).unwrap().unwrap().args,
            vec![
                pos(lit(Value::string("a$b"))),
                pos(lit(Value::string(r"c\d"))),
                pos(lit(Value::string("a b"))),
            ]
        );
        let call = parse(r#"$x("k=v", "a,b", "(1)")"#).unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![
                pos(lit(Value::string("k=v"))),
                pos(lit(Value::string("a,b"))),
                pos(lit(Value::string("(1)"))),
            ]
        );
        for bad in [
            r#"$x("unterminated)"#,
            r#"$x("a" junk)"#,
            r#"$x("\q")"#,
            r#"$x("a\)"#,
        ] {
            let (code, _) = parse(bad).unwrap_err();
            assert_eq!(code, crate::diag::E010, "{bad}");
        }
    }

    #[test]
    fn array_object_literals_parse() {
        let call = parse("$x([1,2])").unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![pos(lit(Value::array(vec![Value::int(1), Value::int(2)])))]
        );
        let call = parse("$x({a: 1})").unwrap().unwrap();
        assert_eq!(call.args.len(), 1);
        if let ParsedValue::Literal(Value::Object(m)) = &call.args[0].value {
            assert_eq!(m.len(), 1);
            assert_eq!(m.get("a"), Some(&Value::int(1)));
        } else {
            panic!("expected object literal, got {:?}", call.args[0].value);
        }
        let call = parse("$x({a:1, b:2})").unwrap().unwrap();
        if let ParsedValue::Literal(Value::Object(m)) = &call.args[0].value {
            assert_eq!(m.len(), 2);
            assert_eq!(m.get("a"), Some(&Value::int(1)));
            assert_eq!(m.get("b"), Some(&Value::int(2)));
        } else {
            panic!("expected object literal, got {:?}", call.args[0].value);
        }
        let call = parse("$x([1,2,3])").unwrap().unwrap();
        if let ParsedValue::Literal(Value::Array(arr)) = &call.args[0].value {
            assert_eq!(
                arr.as_slice(),
                &[Value::int(1), Value::int(2), Value::int(3)]
            );
        } else {
            panic!("expected array literal");
        }
        let call = parse("$x(a=[1])").unwrap().unwrap();
        assert_eq!(call.args[0].key, Some("a".to_string()));
        if let ParsedValue::Literal(Value::Array(arr)) = &call.args[0].value {
            assert_eq!(arr.as_slice(), &[Value::int(1)]);
        } else {
            panic!("expected array literal");
        }
        let call = parse("$x([1], 2)").unwrap().unwrap();
        assert_eq!(call.args.len(), 2);
        assert_eq!(call.args[1].value, lit(Value::int(2)));
    }

    #[test]
    fn nested_call_sites_and_math() {
        let parsed = parse("$x($y(1), ${1+2}, $z, $0)").unwrap().unwrap();
        assert_eq!(
            parsed.args,
            vec![
                pos(call("y", vec![pos(lit(Value::int(1)))])),
                pos(ParsedValue::Math {
                    src: "1+2".to_string()
                }),
                pos(ParsedValue::Ref {
                    name: "z".to_string()
                }),
                pos(ParsedValue::Ref {
                    name: "0".to_string()
                }),
            ]
        );
        let parsed = parse("$x($y(1, $z(2)), 3)").unwrap().unwrap();
        assert_eq!(
            parsed.args,
            vec![
                pos(call(
                    "y",
                    vec![
                        pos(lit(Value::int(1))),
                        pos(call("z", vec![pos(lit(Value::int(2)))])),
                    ]
                )),
                pos(lit(Value::int(3))),
            ]
        );
        let parsed = parse(r#"$x($y("a,b"), ${f(1,2)})"#).unwrap().unwrap();
        assert_eq!(
            parsed.args,
            vec![
                pos(call("y", vec![pos(lit(Value::string("a,b")))])),
                pos(ParsedValue::Math {
                    src: "f(1,2)".to_string()
                }),
            ]
        );
    }

    #[test]
    fn malformed_syntax_is_e010() {
        for bad in [
            "$x(",
            "$x(1",
            "$x(1,",
            "$x(,)",
            "$x(a=)",
            "$x(=1)",
            "$x(1x=2)",
            "$x(a = 1",
            "$x($y(1)z)",
            "$x(${1+2)",
            "$x(\"a)",
            "$x((1)",
        ] {
            let (code, _) = parse(bad).unwrap_err();
            assert_eq!(code, crate::diag::E010, "{bad}");
        }
    }

    #[test]
    fn quotes_and_parens_stay_opaque_in_arg_split() {
        let call = parse(r#"$x("a,b", 1, ${2}, 3)"#).unwrap().unwrap();
        assert_eq!(call.args.len(), 4);
        let call = parse("$x(k=\"a,b\", v=${1,2})").unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![
                named("k", lit(Value::string("a,b"))),
                named(
                    "v",
                    ParsedValue::Math {
                        src: "1,2".to_string()
                    }
                ),
            ]
        );
    }

    #[test]
    fn dollar_tokens_that_are_not_refs_stay_strings() {
        let call = parse("$x($$y, $1x, $x y, $ )").unwrap().unwrap();
        assert_eq!(
            call.args,
            vec![
                pos(ParsedValue::Raw("$$y".to_string())),
                pos(ParsedValue::Raw("$1x".to_string())),
                pos(ParsedValue::Raw("$x y".to_string())),
                pos(ParsedValue::Raw("$".to_string())),
            ]
        );
    }

    // ---- Brace-call payload parser tests (Task 3) ----

    #[test]
    fn brace_payload_object_literal() {
        // `{c: 1, d: 2}` → named args c=1, d=2
        let payload = parse_brace_payload("{c: 1, d: 2}").unwrap();
        assert_eq!(
            payload,
            BracePayload::Object(vec![
                ("c".to_string(), Value::Int(1)),
                ("d".to_string(), Value::Int(2)),
            ])
        );
    }

    #[test]
    fn brace_payload_nested_object() {
        let payload = parse_brace_payload("{a: {b: 1}}").unwrap();
        assert_eq!(
            payload,
            BracePayload::Object(vec![(
                "a".to_string(),
                Value::Object(IndexMap::from([("b".to_string(), Value::Int(1))]))
            )])
        );
    }

    #[test]
    fn brace_payload_array_literal() {
        // `[1, 2]` → positional $0=1, $1=2
        let payload = parse_brace_payload("[1, 2]").unwrap();
        assert_eq!(
            payload,
            BracePayload::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn brace_payload_empty_array() {
        let payload = parse_brace_payload("[]").unwrap();
        assert_eq!(payload, BracePayload::Array(vec![]));
    }

    #[test]
    fn brace_payload_empty_object() {
        let payload = parse_brace_payload("{}").unwrap();
        assert_eq!(payload, BracePayload::Object(vec![]));
    }

    #[test]
    fn brace_payload_scalar_int() {
        // `40` → Int 40
        let payload = parse_brace_payload("40").unwrap();
        assert_eq!(payload, BracePayload::Scalar(Value::Int(40)));
    }

    #[test]
    fn brace_payload_scalar_float() {
        let payload = parse_brace_payload("1.23").unwrap();
        assert_eq!(payload, BracePayload::Scalar(Value::Float(1.23)));
    }

    #[test]
    fn brace_payload_scalar_string() {
        let payload = parse_brace_payload("hello").unwrap();
        assert_eq!(
            payload,
            BracePayload::Scalar(Value::String("hello".to_string()))
        );
    }

    #[test]
    fn brace_payload_scalar_quoted_string() {
        // Quoted string → String with escapes processed
        let payload = parse_brace_payload(r#""hello world""#).unwrap();
        assert_eq!(
            payload,
            BracePayload::Scalar(Value::String("hello world".to_string()))
        );
    }

    #[test]
    fn brace_payload_scalar_single_quoted() {
        let payload = parse_brace_payload("'hello world'").unwrap();
        assert_eq!(
            payload,
            BracePayload::Scalar(Value::String("hello world".to_string()))
        );
    }

    #[test]
    fn brace_payload_scalar_bool() {
        let payload = parse_brace_payload("true").unwrap();
        assert_eq!(payload, BracePayload::Scalar(Value::Bool(true)));
    }

    #[test]
    fn brace_payload_scalar_null() {
        let payload = parse_brace_payload("null").unwrap();
        assert_eq!(payload, BracePayload::Scalar(Value::Null));
    }

    #[test]
    fn brace_payload_preserves_commas_in_string() {
        // A raw string with commas should stay as-is
        let payload = parse_brace_payload("a, b, c").unwrap();
        assert_eq!(
            payload,
            BracePayload::Scalar(Value::String("a, b, c".to_string()))
        );
    }

    #[test]
    fn brace_payload_invalid_key_is_e010() {
        // `{a-b: 1}` → E010 (non-identifier key)
        let (code, msg) = parse_brace_payload("{a-b: 1}").unwrap_err();
        assert_eq!(code, crate::diag::E010);
        assert!(msg.contains("a-b"), "{}", msg);
    }

    #[test]
    fn brace_payload_non_identifier_key_number() {
        // `{123: "x"}` → E010 (numeric key is not a valid identifier in brace-call object)
        let (code, _) = parse_brace_payload("{123: x}").unwrap_err();
        assert_eq!(code, crate::diag::E010);
    }

    #[test]
    fn brace_payload_unbalanced_braces_e010() {
        // A genuinely malformed object key: `: value` (empty key) produces E010.
        let (code, _) = parse_brace_payload("{: 1}").unwrap_err();
        assert_eq!(code, crate::diag::E010);
        // `{a: 1}` (balanced) parses correctly as an object, not a string.
        // `{a: 1, b: 2` with missing closing brace is caught by the scanner
        // (find_matching_brace returns None → E010 at scan time), so the
        // parser never sees it.
    }

    #[test]
    fn brace_payload_unbalanced_brackets_e010() {
        // `[,]` — empty array items parse as empty strings, not an error.
        // Balanced `[1, 2]` parses correctly.
        let payload = parse_brace_payload("[1, 2]").unwrap();
        assert_eq!(
            payload,
            BracePayload::Array(vec![Value::Int(1), Value::Int(2)])
        );
        // `[,]` parses as [String(""), String("")]
        let payload2 = parse_brace_payload("[,]").unwrap();
        assert_eq!(
            payload2,
            BracePayload::Array(vec![
                Value::String("".to_string()),
                Value::String("".to_string())
            ])
        );
        // Note: truly unbalanced brackets like `[1, 2` (missing `]`) are caught
        // by the scanner (find_matching_brace returns None → E010 at scan time),
        // so parse_brace_payload never sees them.
    }

    #[test]
    fn brace_payload_malformed_quoted_e010() {
        let (code, _) = parse_brace_payload(r#""unterminated"#).unwrap_err();
        assert_eq!(code, crate::diag::E010);
    }

    #[test]
    fn brace_payload_nested_array_in_object() {
        let payload = parse_brace_payload("{items: [1, 2, 3]}").unwrap();
        assert_eq!(
            payload,
            BracePayload::Object(vec![(
                "items".to_string(),
                Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
            )])
        );
    }

    #[test]
    fn brace_payload_complex_object_with_quoted_values() {
        let payload = parse_brace_payload(r#"{name: "Alice", greeting: 'Hello, world!'}"#).unwrap();
        assert_eq!(
            payload,
            BracePayload::Object(vec![
                ("name".to_string(), Value::String("Alice".to_string())),
                (
                    "greeting".to_string(),
                    Value::String("Hello, world!".to_string())
                ),
            ])
        );
    }

    #[test]
    fn brace_payload_whitespace_trimmed() {
        // Payload source is pre-trimmed by scanner, but verify the parser handles it
        let payload = parse_brace_payload("  {a: 1}  ").unwrap();
        assert_eq!(
            payload,
            BracePayload::Object(vec![("a".to_string(), Value::Int(1))])
        );
    }
}
