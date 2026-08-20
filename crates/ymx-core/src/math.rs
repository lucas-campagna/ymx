//! Math engine boundary, evaluation scope, and the v1 dynamic evaluator.
//!
//! [`MathEngine`] is the trait behind `${...}` evaluation — the seam for
//! swapping in a future Lua/Python/JavaScript engine (PRD *Architecture:
//! Math*). [`V1Engine`] implements it with a lexer/parser/evaluator for the
//! rule-7 grammar: `+ - * / % **`, unary `-`, parens, decimal Int/Float
//! literals, bare-identifier / `$N` argument references, and `name(...)`
//! component calls (dotted namespace paths allowed). Precedence (highest to
//! lowest): `**` (right-associative), then unary `-`, then `* / %`
//! (left-associative), then `+ -` (left-associative). There are **no**
//! comparison, equality, or boolean operators — they are literal text in
//! strings (rule 13) — and no quoted string literals in v1; any token outside
//! the grammar is `E010`. String-valued operands are re-parsed and evaluated
//! as math in the current scope ([`resolve_operand`], *Math operand
//! resolution* in the PRD); a String that does not parse stays a plain
//! operand (`+` concatenates, numeric operators raise `E011`).
//!
//! [`Scope`] carries the in-scope named/positional arguments, the reduce-step
//! `last` value, the component-call dispatch hook for math `name(...)` calls
//! (wired by the resolver in milestone 1.6, including the `E008` depth check
//! at the call boundary), and the diagnostic context.

use std::path::PathBuf;
use std::rc::Rc;

use crate::callsite;
use crate::diag::{Diagnostic, Span, E002, E003, E008, E010, E011};
use crate::ir::{render_value, Value};

/// The math-engine boundary: evaluates the contents of a `${...}` expression.
///
/// `src` is the raw text between the braces; `scope` supplies the in-scope
/// arguments (named + positional), the reduce-step `last` value, and the
/// component-call hook. The trait is the boundary for swapping to a
/// Lua/Python/JavaScript engine in the future (PRD *Architecture: Math*).
pub trait MathEngine {
    fn eval(&self, src: &str, scope: &Scope<'_>) -> Result<Value, Diagnostic>;
}

/// Component-call dispatch hook for math `name(...)` calls: receives the
/// (possibly dotted) name and the positional argument values. `Rc` keeps
/// [`Scope`] clonable (the resolver builds child scopes per chain/dispatch
/// step).
pub type CallHook<'a> = Rc<dyn Fn(&str, &[Value]) -> Result<Value, Diagnostic> + 'a>;

/// Component-call dispatch hook for shell interpolation `$name(args)` calls.
pub type ShellCallHook<'a> =
    Rc<dyn Fn(&callsite::ParsedCall, &Scope<'a>, Span) -> Result<Value, Diagnostic> + 'a>;

/// Evaluation scope for `${...}` math and string interpolation.
///
/// Carries the named arguments (rule 2), the positional arguments (rule 4),
/// the previous step's result in a reduce (`last`, rules 13/16 — `None`
/// outside a reduce step or on its first step), the component-call dispatch
/// hook for math `name(...)` calls (wired by the resolver in milestone 1.6,
/// including the `E008` depth check at the call boundary), and the
/// diagnostic context (`file`, `component`, and the base `span` math
/// diagnostics are attributed to).
#[derive(Clone)]
pub struct Scope<'a> {
    /// Resolved host-file path for diagnostics.
    pub file: Option<PathBuf>,
    /// Compiling component name for diagnostics.
    pub component: Option<String>,
    /// Base span (line/col) diagnostics are attributed to; offsets inside a
    /// `${...}` source are computed relative to this.
    pub span: Span,
    /// Named arguments, in binding order (`lookup` returns the first match).
    pub named: Vec<(String, Value)>,
    /// Positional arguments (`$0`, `$1`, …).
    pub positional: Vec<Value>,
    /// Previous step's result in a reduce context; `None` outside a reduce or
    /// on its first step. Referencing `last` with `None` here (and no named
    /// argument `last` in scope) is `E003`.
    pub last: Option<Value>,
    /// Component-call dispatch hook for math `name(...)` calls ([`CallHook`]).
    /// `None` means no hook is registered (`invoke` then reports `E002`).
    pub call: Option<CallHook<'a>>,
    /// Component-call dispatch hook for shell interpolation `$name(args)`.
    pub shell_call: Option<ShellCallHook<'a>>,
}

impl<'a> Default for Scope<'a> {
    fn default() -> Self {
        Scope::new()
    }
}

impl<'a> Scope<'a> {
    /// Empty scope: no arguments, not in a reduce, no diagnostic context.
    pub fn new() -> Scope<'a> {
        Scope {
            file: None,
            component: None,
            span: Span { line: 1, col: 1 },
            named: Vec::new(),
            positional: Vec::new(),
            last: None,
            call: None,
            shell_call: None,
        }
    }

    /// Scope for a plain (non-reduce) call with `named` and `positional`
    /// arguments.
    pub fn with_args(named: Vec<(String, Value)>, positional: Vec<Value>) -> Scope<'a> {
        Scope {
            named,
            positional,
            ..Scope::new()
        }
    }

    /// Scope for a reduce step (rules 13/16) with `last` bound to the previous
    /// step's result. The first step — or any evaluation outside a reduce —
    /// uses [`Scope::with_args`] / [`Scope::new`] instead, leaving `last`
    /// unset so that referencing `last` is `E003`.
    pub fn reduce_step(
        named: Vec<(String, Value)>,
        positional: Vec<Value>,
        last: Value,
    ) -> Scope<'a> {
        Scope {
            named,
            positional,
            last: Some(last),
            ..Scope::new()
        }
    }

    /// Resolve an argument reference: named arguments first (first match
    /// wins), then the reduce-step `last` (a named argument `last` shadows it
    /// — `last` is an ordinary in-scope argument, nothing more).
    pub fn lookup(&self, name: &str) -> Option<&Value> {
        self.named
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
            .or_else(|| {
                if name == "last" {
                    self.last.as_ref()
                } else {
                    None
                }
            })
    }

    /// The positional argument `$index`.
    pub fn positional_at(&self, index: usize) -> Option<&Value> {
        self.positional.get(index)
    }

    /// Dispatch a math `name(...)` component call through the registered
    /// hook. Without a hook the component cannot be reached: `E002`.
    pub fn invoke(&self, name: &str, args: &[Value]) -> Result<Value, Diagnostic> {
        match &self.call {
            Some(f) => f(name, args),
            None => Err(Diagnostic {
                file: self.file.clone(),
                line: self.span.line,
                col: self.span.col,
                component: self.component.clone(),
                code: E002,
                message: format!(
                    "unknown component reference `{name}` (no component-call hook registered)"
                ),
            }),
        }
    }

    /// Dispatch a shell interpolation `$name(args)` call through the
    /// registered hook.
    pub fn invoke_shell(
        &self,
        call: &callsite::ParsedCall,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match &self.shell_call {
            Some(f) => f(call, self, span),
            None => Err(Diagnostic {
                file: self.file.clone(),
                line: span.line,
                col: span.col,
                component: self.component.clone(),
                code: E002,
                message: format!(
                    "unknown component reference `{}` (no shell component-call hook registered)",
                    call.name
                ),
            }),
        }
    }
}

/// The v1 dynamic evaluator: lexer/parser/evaluator for the rule-7 grammar.
#[derive(Debug, Clone, Copy, Default)]
pub struct V1Engine;

impl MathEngine for V1Engine {
    fn eval(&self, src: &str, scope: &Scope) -> Result<Value, Diagnostic> {
        eval_inner(src, scope, 0)
    }
}

/// Bound on String re-scan nesting ([`resolve_operand`]): a self-referential
/// string (e.g. `x = "x"`) cannot recurse forever and aborts with `E008`
/// instead of overflowing the stack. The rule-11 depth cap is wired by the
/// resolver (milestone 1.6) at component-call boundaries; this bounds the
/// math-internal re-scan loop.
const RESCAN_LIMIT: u32 = 256;

fn eval_inner(src: &str, scope: &Scope, depth: u32) -> Result<Value, Diagnostic> {
    let expr = parse(src).map_err(|(offset, message)| err(scope, src, offset, E010, message))?;
    eval_expr(&expr, src, scope, depth)
}

// ---- Lexer ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(Value),
    Ident(String),
    Dollar(usize),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    LParen,
    RParen,
    Comma,
}

/// Lex `src` into tokens with their byte offsets. `Err((offset, message))` is
/// an `E010` syntax error at that offset: `$` must be followed by digits
/// (`$letter` is `E010` — drop the `$` to reference a named argument), number
/// literals are decimal Int/Float, identifiers may be dotted (`subdir.comp`)
/// for call names, and any other character (including `<`, `>`, `=`, quotes,
/// backslash) is rejected.
fn lex(src: &str) -> Result<Vec<(Tok, usize)>, (usize, String)> {
    let bytes = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            b'+' => {
                toks.push((Tok::Plus, i));
                i += 1;
            }
            b'-' => {
                toks.push((Tok::Minus, i));
                i += 1;
            }
            b'*' => {
                if bytes.get(i + 1) == Some(&b'*') {
                    toks.push((Tok::StarStar, i));
                    i += 2;
                } else {
                    toks.push((Tok::Star, i));
                    i += 1;
                }
            }
            b'/' => {
                toks.push((Tok::Slash, i));
                i += 1;
            }
            b'%' => {
                toks.push((Tok::Percent, i));
                i += 1;
            }
            b'(' => {
                toks.push((Tok::LParen, i));
                i += 1;
            }
            b')' => {
                toks.push((Tok::RParen, i));
                i += 1;
            }
            b',' => {
                toks.push((Tok::Comma, i));
                i += 1;
            }
            b'$' => {
                let start = i;
                i += 1;
                let digits_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i == digits_start {
                    return Err((
                        start,
                        "`$` inside `${...}` must be followed by digits (`$0`, `$1`, …); drop the `$` to reference a named argument"
                            .to_string(),
                    ));
                }
                match src[digits_start..i].parse::<usize>() {
                    Ok(index) => toks.push((Tok::Dollar(index), start)),
                    Err(_) => return Err((start, "positional index is too large".to_string())),
                }
            }
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let mut is_float = false;
                if bytes.get(i) == Some(&b'.')
                    && bytes.get(i + 1).is_some_and(|n| n.is_ascii_digit())
                {
                    is_float = true;
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
                    let mut j = i + 1;
                    if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
                        j += 1;
                    }
                    if bytes.get(j).is_some_and(|n| n.is_ascii_digit()) {
                        is_float = true;
                        i = j;
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let text = &src[start..i];
                if is_float {
                    match text.parse::<f64>() {
                        Ok(f) => toks.push((Tok::Num(Value::Float(f)), start)),
                        Err(_) => {
                            return Err((start, format!("invalid number literal `{text}`")));
                        }
                    }
                } else {
                    match text.parse::<i64>() {
                        Ok(n) => toks.push((Tok::Num(Value::Int(n)), start)),
                        Err(_) => match text.parse::<f64>() {
                            Ok(f) => toks.push((Tok::Num(Value::Float(f)), start)),
                            Err(_) => {
                                return Err((start, format!("invalid number literal `{text}`")));
                            }
                        },
                    }
                }
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                while bytes.get(i) == Some(&b'.')
                    && bytes
                        .get(i + 1)
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                {
                    i += 2;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                }
                toks.push((Tok::Ident(src[start..i].to_string()), start));
            }
            _ => {
                return Err((
                    i,
                    format!("unexpected character `{}` in math expression", c as char),
                ));
            }
        }
    }
    Ok(toks)
}

// ---- Parser ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Num(Value, usize),
    Arg {
        name: String,
        offset: usize,
    },
    Pos {
        index: usize,
        offset: usize,
    },
    Call {
        name: String,
        args: Vec<Expr>,
        offset: usize,
    },
    Neg {
        inner: Box<Expr>,
        offset: usize,
    },
    Bin {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        offset: usize,
    },
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    last_offset: usize,
}

/// Parse a fully-lexed expression. `Err((offset, message))` is an `E010`
/// syntax error at that offset. Precedence per PRD rule 7: `**`
/// (right-associative) > unary `-` > `* / %` (left-associative) >
/// `+ -` (left-associative); parentheses group.
fn parse(src: &str) -> Result<Expr, (usize, String)> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err((0, "empty math expression".to_string()));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        last_offset: 0,
    };
    let expr = p.parse_expr()?;
    if p.pos < p.toks.len() {
        let offset = p.toks[p.pos].1;
        return Err((
            offset,
            "unexpected trailing input in math expression".to_string(),
        ));
    }
    Ok(expr)
}

impl Parser {
    fn peek(&self) -> Option<(Tok, usize)> {
        self.toks.get(self.pos).cloned()
    }

    fn next(&mut self) -> Option<(Tok, usize)> {
        let t = self.toks.get(self.pos).cloned();
        if let Some((_, offset)) = &t {
            self.last_offset = *offset;
            self.pos += 1;
        }
        t
    }

    /// Offset of the next unconsumed token, or of the last consumed one when
    /// the input is exhausted (so end-of-input errors point at the operator
    /// that started the dangling construct).
    fn end_offset(&self) -> usize {
        self.toks
            .get(self.pos)
            .map(|(_, o)| *o)
            .unwrap_or(self.last_offset)
    }

    fn parse_expr(&mut self) -> Result<Expr, (usize, String)> {
        let mut left = self.parse_term()?;
        loop {
            match self.peek() {
                Some((Tok::Plus, offset)) => {
                    self.pos += 1;
                    let right = self.parse_term()?;
                    left = Expr::Bin {
                        op: BinOp::Add,
                        left: Box::new(left),
                        right: Box::new(right),
                        offset,
                    };
                }
                Some((Tok::Minus, offset)) => {
                    self.pos += 1;
                    let right = self.parse_term()?;
                    left = Expr::Bin {
                        op: BinOp::Sub,
                        left: Box::new(left),
                        right: Box::new(right),
                        offset,
                    };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<Expr, (usize, String)> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some((Tok::Star, offset)) => {
                    self.pos += 1;
                    let right = self.parse_unary()?;
                    left = Expr::Bin {
                        op: BinOp::Mul,
                        left: Box::new(left),
                        right: Box::new(right),
                        offset,
                    };
                }
                Some((Tok::Slash, offset)) => {
                    self.pos += 1;
                    let right = self.parse_unary()?;
                    left = Expr::Bin {
                        op: BinOp::Div,
                        left: Box::new(left),
                        right: Box::new(right),
                        offset,
                    };
                }
                Some((Tok::Percent, offset)) => {
                    self.pos += 1;
                    let right = self.parse_unary()?;
                    left = Expr::Bin {
                        op: BinOp::Rem,
                        left: Box::new(left),
                        right: Box::new(right),
                        offset,
                    };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, (usize, String)> {
        match self.peek() {
            Some((Tok::Minus, offset)) => {
                self.pos += 1;
                let inner = self.parse_unary()?;
                Ok(Expr::Neg {
                    inner: Box::new(inner),
                    offset,
                })
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<Expr, (usize, String)> {
        let base = self.parse_atom()?;
        match self.peek() {
            Some((Tok::StarStar, offset)) => {
                self.pos += 1;
                let exponent = self.parse_unary()?;
                Ok(Expr::Bin {
                    op: BinOp::Pow,
                    left: Box::new(base),
                    right: Box::new(exponent),
                    offset,
                })
            }
            _ => Ok(base),
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, (usize, String)> {
        let Some((tok, offset)) = self.next() else {
            return Err((self.end_offset(), "expected an expression".to_string()));
        };
        match tok {
            Tok::Num(v) => Ok(Expr::Num(v, offset)),
            Tok::Dollar(index) => Ok(Expr::Pos { index, offset }),
            Tok::Ident(name) => {
                if matches!(self.peek(), Some((Tok::LParen, _))) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some((Tok::RParen, _))) {
                        loop {
                            args.push(self.parse_expr()?);
                            match self.peek() {
                                Some((Tok::Comma, _)) => {
                                    self.pos += 1;
                                }
                                Some((Tok::RParen, _)) => break,
                                _ => {
                                    return Err((
                                        self.end_offset(),
                                        "expected `,` or `)` in component call".to_string(),
                                    ));
                                }
                            }
                        }
                    }
                    if !matches!(self.next(), Some((Tok::RParen, _))) {
                        return Err((offset, "expected `)` in component call".to_string()));
                    }
                    Ok(Expr::Call { name, args, offset })
                } else if name.contains('.') {
                    Err((
                        offset,
                        format!("dotted identifier `{name}` must be a component call"),
                    ))
                } else {
                    Ok(Expr::Arg { name, offset })
                }
            }
            Tok::LParen => {
                let inner = self.parse_expr()?;
                match self.next() {
                    Some((Tok::RParen, _)) => Ok(inner),
                    _ => Err((offset, "expected `)`".to_string())),
                }
            }
            _ => Err((offset, "expected an expression".to_string())),
        }
    }
}

// ---- Evaluator ----

fn eval_expr(e: &Expr, src: &str, scope: &Scope, depth: u32) -> Result<Value, Diagnostic> {
    match e {
        Expr::Num(v, _) => Ok(v.clone()),
        Expr::Arg { name, offset } => {
            let v = match scope.lookup(name) {
                Some(v) => v.clone(),
                None => {
                    return Err(err(
                        scope,
                        src,
                        *offset,
                        E003,
                        format!("missing required argument `{name}`"),
                    ));
                }
            };
            resolve_operand(v, src, scope, *offset, depth)
        }
        Expr::Pos { index, offset } => {
            let v = match scope.positional_at(*index) {
                Some(v) => v.clone(),
                None => {
                    return Err(err(
                        scope,
                        src,
                        *offset,
                        E003,
                        format!("missing required argument `${index}`"),
                    ));
                }
            };
            resolve_operand(v, src, scope, *offset, depth)
        }
        Expr::Call { name, args, .. } => {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(eval_expr(arg, src, scope, depth)?);
            }
            scope.invoke(name, &values)
        }
        Expr::Neg { inner, offset } => {
            let v = resolve_operand(
                eval_expr(inner, src, scope, depth)?,
                src,
                scope,
                *offset,
                depth,
            )?;
            match v {
                Value::Int(i) => match i.checked_neg() {
                    Some(n) => Ok(Value::Int(n)),
                    None => Ok(Value::Float(-(i as f64))),
                },
                Value::Float(f) => Ok(Value::Float(-f)),
                _ => Err(err(
                    scope,
                    src,
                    *offset,
                    E011,
                    "unary `-` requires a number".to_string(),
                )),
            }
        }
        Expr::Bin {
            op,
            left,
            right,
            offset,
        } => {
            let l = resolve_operand(
                eval_expr(left, src, scope, depth)?,
                src,
                scope,
                *offset,
                depth,
            )?;
            let r = resolve_operand(
                eval_expr(right, src, scope, depth)?,
                src,
                scope,
                *offset,
                depth,
            )?;
            apply_bin(op, l, r, src, scope, *offset)
        }
    }
}

/// Re-scan a String-valued operand as a math expression in the current scope.
///
/// PRD *Math operand resolution*: when an operand — a bare identifier,
/// `last`, or a `$N` positional — resolves to a String, that String is
/// re-parsed and evaluated as math in the **current** scope (the same scope
/// as the enclosing `${...}`, including `last` and all in-scope arguments):
/// `"1 + 2"` → `3`, `"123"` → `123`, `"x + 1"` (with `x` in scope) →
/// `x + 1`. A String that does **not** parse as math (free text like
/// `"hello"`) is left as a plain String operand of the surrounding operator
/// (numeric operators then raise `E011`; `+` concatenates). Non-String
/// operands are used directly. Applies uniformly to *every* String-valued
/// operand in math, including a single-operand whole expression (`${last}`
/// with `last = "1 + 2"` → `3`); only a parse failure (`E010`) keeps the
/// String — errors from evaluating the re-scanned expression propagate.
///
/// Gotcha: because re-scan evaluates in the current scope, a String argument
/// whose content is a bare-identifier-looking token resolves to that
/// identifier. E.g. `${ x }` with `x = "y"` re-scans as `y`, which looks up
/// the argument `y` (→ `E003` if absent). Keep String arguments used in math
/// either numeric or full math expressions; avoid re-using argument names as
/// string contents.
///
/// Re-scan recursion is bounded by [`RESCAN_LIMIT`]: a self-referential
/// string (e.g. `x = "x"`) aborts with `E008` instead of overflowing.
fn resolve_operand(
    v: Value,
    src: &str,
    scope: &Scope,
    offset: usize,
    depth: u32,
) -> Result<Value, Diagnostic> {
    match v {
        Value::String(s) => {
            if depth >= RESCAN_LIMIT {
                return Err(err(
                    scope,
                    src,
                    offset,
                    E008,
                    "max-depth exceeded during math string re-scan".to_string(),
                ));
            }
            match eval_inner(&s, scope, depth + 1) {
                Ok(v) => Ok(v),
                Err(d) if d.code == E010 => Ok(Value::string(s)),
                Err(d) => Err(d),
            }
        }
        v => Ok(v),
    }
}

fn apply_bin(
    op: &BinOp,
    l: Value,
    r: Value,
    src: &str,
    scope: &Scope,
    offset: usize,
) -> Result<Value, Diagnostic> {
    match op {
        BinOp::Add => add(l, r, src, scope, offset),
        BinOp::Sub => num_only(
            "-",
            l,
            r,
            |a, b| match (a, b) {
                (Num::Int(x), Num::Int(y)) => x
                    .checked_sub(y)
                    .map(Value::Int)
                    .unwrap_or_else(|| Value::Float(x as f64 - y as f64)),
                (a, b) => Value::Float(to_f64(a) - to_f64(b)),
            },
            src,
            scope,
            offset,
        ),
        BinOp::Mul => num_only(
            "*",
            l,
            r,
            |a, b| match (a, b) {
                (Num::Int(x), Num::Int(y)) => x
                    .checked_mul(y)
                    .map(Value::Int)
                    .unwrap_or_else(|| Value::Float(x as f64 * y as f64)),
                (a, b) => Value::Float(to_f64(a) * to_f64(b)),
            },
            src,
            scope,
            offset,
        ),
        BinOp::Div => div(l, r, src, scope, offset),
        BinOp::Rem => rem(l, r, src, scope, offset),
        BinOp::Pow => pow(l, r, src, scope, offset),
    }
}

/// A numeric operand: Int or Float (Bool and Null are not numbers).
#[derive(Debug, Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

fn as_num(v: &Value) -> Option<Num> {
    match v {
        Value::Int(i) => Some(Num::Int(*i)),
        Value::Float(f) => Some(Num::Float(*f)),
        _ => None,
    }
}

fn to_f64(n: Num) -> f64 {
    match n {
        Num::Int(i) => i as f64,
        Num::Float(f) => f,
    }
}

/// Coerce a number to Int for `%`: floats truncate toward zero (Rust `as`).
fn to_int(n: Num) -> i64 {
    match n {
        Num::Int(i) => i,
        Num::Float(f) => f as i64,
    }
}

/// A numeric-only binary operator: both operands must be numbers (`E011`
/// otherwise); Int ⊕ Int stays Int unless the operation overflows (falls back
/// to Float); any Float operand promotes the result to Float.
fn num_only(
    op: &str,
    l: Value,
    r: Value,
    f: impl FnOnce(Num, Num) -> Value,
    src: &str,
    scope: &Scope,
    offset: usize,
) -> Result<Value, Diagnostic> {
    match (as_num(&l), as_num(&r)) {
        (Some(a), Some(b)) => Ok(f(a, b)),
        _ => Err(err(
            scope,
            src,
            offset,
            E011,
            format!("non-numeric operand for `{op}`"),
        )),
    }
}

/// `+` semantics (PRD rule 7): both numbers → numeric add (Float if either is
/// Float); both Strings → concatenation; one String + one number → the number
/// rendered via the shared [`render_value`] helper, then concatenated; any
/// other mixture (Bool, Null, Array, Object) is `E011`.
fn add(l: Value, r: Value, src: &str, scope: &Scope, offset: usize) -> Result<Value, Diagnostic> {
    match (&l, &r) {
        (Value::Int(x), Value::Int(y)) => Ok(x
            .checked_add(*y)
            .map(Value::Int)
            .unwrap_or_else(|| Value::Float(*x as f64 + *y as f64))),
        (Value::Int(x), Value::Float(y)) => Ok(Value::Float(*x as f64 + y)),
        (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x + *y as f64)),
        (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x + y)),
        (Value::String(x), Value::String(y)) => Ok(Value::string(format!("{x}{y}"))),
        (Value::String(x), n @ (Value::Int(_) | Value::Float(_))) => {
            let s = render_value(n).expect("Int and Float always render");
            Ok(Value::string(format!("{x}{s}")))
        }
        (n @ (Value::Int(_) | Value::Float(_)), Value::String(y)) => {
            let s = render_value(n).expect("Int and Float always render");
            Ok(Value::string(format!("{s}{y}")))
        }
        _ => Err(err(
            scope,
            src,
            offset,
            E011,
            "unsupported operand mixture for `+`".to_string(),
        )),
    }
}

/// `/`: always floating-point division; division by zero is `E011`.
fn div(l: Value, r: Value, src: &str, scope: &Scope, offset: usize) -> Result<Value, Diagnostic> {
    match (as_num(&l), as_num(&r)) {
        (Some(a), Some(b)) => {
            let d = to_f64(b);
            if d == 0.0 {
                Err(err(
                    scope,
                    src,
                    offset,
                    E011,
                    "division by zero".to_string(),
                ))
            } else {
                Ok(Value::Float(to_f64(a) / d))
            }
        }
        _ => Err(err(
            scope,
            src,
            offset,
            E011,
            "non-numeric operand for `/`".to_string(),
        )),
    }
}

/// `%`: both operands coerce to Int (floats truncate toward zero; non-numeric
/// → `E011`); the sign follows the dividend (Rust `i64::rem`); `% 0` is
/// `E011`.
fn rem(l: Value, r: Value, src: &str, scope: &Scope, offset: usize) -> Result<Value, Diagnostic> {
    match (as_num(&l), as_num(&r)) {
        (Some(a), Some(b)) => {
            let (x, y) = (to_int(a), to_int(b));
            if y == 0 {
                Err(err(scope, src, offset, E011, "modulo by zero".to_string()))
            } else {
                Ok(Value::Int(x % y))
            }
        }
        _ => Err(err(
            scope,
            src,
            offset,
            E011,
            "non-numeric operand for `%`".to_string(),
        )),
    }
}

/// `**`: Int ** Int(non-negative) → Int via `checked_pow` (on overflow falls
/// back to Float — never panics); negative or fractional exponents and any
/// Float operand → Float.
fn pow(l: Value, r: Value, src: &str, scope: &Scope, offset: usize) -> Result<Value, Diagnostic> {
    match (as_num(&l), as_num(&r)) {
        (Some(a), Some(b)) => match (a, b) {
            (Num::Int(x), Num::Int(y)) if y >= 0 => {
                match u32::try_from(y).ok().and_then(|e| x.checked_pow(e)) {
                    Some(p) => Ok(Value::Int(p)),
                    None => Ok(Value::Float((x as f64).powf(y as f64))),
                }
            }
            (a, b) => Ok(Value::Float(to_f64(a).powf(to_f64(b)))),
        },
        _ => Err(err(
            scope,
            src,
            offset,
            E011,
            "non-numeric operand for `**`".to_string(),
        )),
    }
}

/// A math diagnostic attributed to `scope`'s file/component context at byte
/// `offset` inside `src` (relative to `scope.span`).
fn err(scope: &Scope, src: &str, offset: usize, code: &'static str, message: String) -> Diagnostic {
    let span = span_at(scope.span, src, offset);
    Diagnostic {
        file: scope.file.clone(),
        line: span.line,
        col: span.col,
        component: scope.component.clone(),
        code,
        message,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn eval_ok(src: &str, scope: &Scope<'_>) -> Value {
        V1Engine
            .eval(src, scope)
            .unwrap_or_else(|d| panic!("{src}: {}", d.message))
    }

    fn eval_err(src: &str, scope: &Scope<'_>) -> Diagnostic {
        match V1Engine.eval(src, scope) {
            Err(d) => d,
            Ok(v) => panic!("{src}: expected error, got {v:?}"),
        }
    }

    fn hook(f: impl Fn(&str, &[Value]) -> Result<Value, Diagnostic> + 'static) -> Scope<'static> {
        Scope {
            call: Some(Rc::new(f)),
            shell_call: None,
            ..Scope::new()
        }
    }

    #[test]
    fn lookup_prefers_named_argument_over_last() {
        let mut scope = Scope::with_args(vec![("last".to_string(), Value::int(1))], vec![]);
        assert_eq!(scope.lookup("last"), Some(&Value::int(1)));
        scope.last = Some(Value::int(2));
        assert_eq!(
            scope.lookup("last"),
            Some(&Value::int(1)),
            "a named argument `last` shadows the reduce result"
        );
    }

    #[test]
    fn lookup_falls_back_to_reduce_last() {
        let scope = Scope::reduce_step(vec![], vec![], Value::int(6));
        assert_eq!(scope.lookup("last"), Some(&Value::int(6)));
        assert_eq!(scope.lookup("x"), None);
        assert_eq!(scope.positional_at(0), None);
    }

    // ---- `last` availability (rules 13/16) ----

    #[test]
    fn last_outside_reduce_is_e003() {
        assert_eq!(eval_err("last", &Scope::new()).code, E003);
        let scope = Scope::with_args(vec![("x".to_string(), Value::int(1))], vec![]);
        for src in [
            "last", "last + 1", "1 + last", "-last", "last * 2", "2 / last",
        ] {
            assert_eq!(eval_err(src, &scope).code, E003, "{src}");
        }
    }

    #[test]
    fn last_in_reduce_step_is_available() {
        let scope = Scope::reduce_step(
            vec![("x".to_string(), Value::int(2))],
            vec![],
            Value::int(5),
        );
        assert_eq!(eval_ok("last", &scope), Value::int(5));
        assert_eq!(eval_ok("x + last", &scope), Value::int(7));
    }

    #[test]
    fn positional_at_indexes_positional_args() {
        let scope = Scope::with_args(vec![], vec![Value::int(12), Value::int(34)]);
        assert_eq!(scope.positional_at(0), Some(&Value::int(12)));
        assert_eq!(scope.positional_at(1), Some(&Value::int(34)));
        assert_eq!(scope.positional_at(2), None);
    }

    #[test]
    fn invoke_without_hook_is_e002() {
        let scope = Scope::new();
        let err = scope.invoke("b", &[Value::int(1)]).unwrap_err();
        assert_eq!(err.code, E002);
        assert!(err.message.contains("b"));
    }

    #[test]
    fn invoke_dispatches_through_hook() {
        let scope = hook(|name: &str, args: &[Value]| {
            let sum: i64 = args
                .iter()
                .map(|v| match v {
                    Value::Int(i) => *i,
                    _ => 0,
                })
                .sum();
            Ok(Value::int(sum + name.len() as i64))
        });
        assert_eq!(
            scope
                .invoke("b", &[Value::int(12), Value::int(34)])
                .unwrap(),
            Value::int(47)
        );
    }

    // ---- v1 evaluator ----

    #[test]
    fn positional_operands_sum_to_46() {
        let scope = Scope::with_args(vec![], vec![Value::int(12), Value::int(34)]);
        assert_eq!(eval_ok("$0 + $1", &scope), Value::int(46));
    }

    #[test]
    fn component_calls_dispatch_through_scope_hook() {
        let scope = hook(|name, args| {
            let sum: i64 = args
                .iter()
                .map(|v| match v {
                    Value::Int(i) => *i,
                    _ => 0,
                })
                .sum();
            match name {
                "c" => Ok(Value::int(2 * sum)),
                _ => Ok(Value::int(sum)),
            }
        });
        assert_eq!(eval_ok("b(12,34) + c(28)", &scope), Value::int(102));
        assert_eq!(eval_ok("b()", &scope), Value::int(0));
        assert_eq!(eval_ok("subdir.comp(1,2)", &scope), Value::int(3));
    }

    #[test]
    fn call_arguments_evaluate_as_expressions() {
        let scope = hook(|name, args| {
            assert_eq!(name, "b");
            Ok(Value::array(args.to_vec()))
        });
        assert_eq!(
            eval_ok("b(1 + 2, 3 * 4)", &scope),
            Value::array(vec![Value::int(3), Value::int(12)])
        );
        assert_eq!(
            eval_ok("b(b(1), 2)", &scope),
            Value::array(vec![Value::array(vec![Value::int(1)]), Value::int(2)])
        );
    }

    #[test]
    fn precedence_matches_prd() {
        let empty = Scope::new();
        assert_eq!(eval_ok("2 + 3 * 4", &empty), Value::int(14));
        assert_eq!(eval_ok("2 * 3 + 4", &empty), Value::int(10));
        assert_eq!(eval_ok("2 + 3 * 4 ** 2", &empty), Value::int(50));
        assert_eq!(eval_ok("(2 + 3) * 4", &empty), Value::int(20));
        assert_eq!(eval_ok("(1 + 2) * (3 + 4)", &empty), Value::int(21));
        assert_eq!(eval_ok("-2 ** 2", &empty), Value::int(-4));
        assert_eq!(eval_ok("(-2) ** 2", &empty), Value::int(4));
        assert_eq!(eval_ok("2 ** -2", &empty), Value::float(0.25));
        assert_eq!(eval_ok("3 ** 3 ** 3", &empty), Value::int(7625597484987));
        assert_eq!(eval_ok("2 + 3 ** 2 * 4", &empty), Value::int(38));
    }

    #[test]
    fn division_is_always_float() {
        let empty = Scope::new();
        assert_eq!(eval_ok("5 / 2", &empty), Value::float(2.5));
        assert_eq!(eval_ok("5 / 2.0", &empty), Value::float(2.5));
        assert_eq!(eval_ok("6 / 3", &empty), Value::float(2.0));
    }

    #[test]
    fn division_by_zero_is_e011() {
        let empty = Scope::new();
        for src in ["5 / 0", "5.0 / 0", "5 / 0.0", "0 / 0"] {
            let d = eval_err(src, &empty);
            assert_eq!(d.code, E011, "{src}");
            assert!(d.message.contains("zero"), "{src}: {}", d.message);
        }
    }

    #[test]
    fn remainder_coerces_to_int_and_follows_dividend_sign() {
        let empty = Scope::new();
        assert_eq!(eval_ok("7 % 3", &empty), Value::int(1));
        assert_eq!(eval_ok("-7 % 3", &empty), Value::int(-1));
        assert_eq!(eval_ok("7 % -3", &empty), Value::int(1));
        assert_eq!(eval_ok("5.9 % 2", &empty), Value::int(1));
        assert_eq!(eval_ok("-5.9 % 2", &empty), Value::int(-1));
        assert_eq!(eval_err("5 % 0", &empty).code, E011);
        assert_eq!(eval_err("5 % 0.0", &empty).code, E011);
        let scope = Scope::with_args(vec![("s".to_string(), Value::string("x y"))], vec![]);
        assert_eq!(eval_err("s % 2", &scope).code, E011);
    }

    #[test]
    fn exponentiation_int_vs_float() {
        let empty = Scope::new();
        assert_eq!(eval_ok("2 ** 10", &empty), Value::int(1024));
        assert_eq!(eval_ok("2 ** 0", &empty), Value::int(1));
        assert_eq!(eval_ok("0 ** 0", &empty), Value::int(1));
        assert_eq!(eval_ok("2 ** 62", &empty), Value::int(4611686018427387904));
        assert_eq!(
            eval_ok("2 ** 63", &empty),
            Value::float(9.223372036854776e18)
        );
        assert_eq!(eval_ok("2 ** -1", &empty), Value::float(0.5));
        assert_eq!(eval_ok("2.0 ** 2", &empty), Value::float(4.0));
        assert_eq!(eval_ok("2 ** 2.5", &empty), Value::float(5.656854249492381));
        assert_eq!(eval_ok("9 ** 0.5", &empty), Value::float(3.0));
    }

    #[test]
    fn add_numeric_promotion_and_overflow() {
        let empty = Scope::new();
        assert_eq!(eval_ok("1 + 2", &empty), Value::int(3));
        assert_eq!(eval_ok("1 + 2.5", &empty), Value::float(3.5));
        assert_eq!(eval_ok("0.5 + 1", &empty), Value::float(1.5));
        assert_eq!(
            eval_ok("9223372036854775807 + 1", &empty),
            Value::float(9.223372036854776e18)
        );
        assert_eq!(
            eval_ok("9223372036854775807 - 1", &empty),
            Value::int(9223372036854775806)
        );
        assert_eq!(eval_ok("7 - 2.5", &empty), Value::float(4.5));
        assert_eq!(eval_ok("7 * 2", &empty), Value::int(14));
        assert_eq!(eval_ok("2.5 * 2", &empty), Value::float(5.0));
        assert_eq!(
            eval_ok("9223372036854775807 * 2", &empty),
            Value::float(1.8446744073709552e19)
        );
    }

    #[test]
    fn add_string_concatenation_semantics() {
        // Non-parseable string operands are left as Strings and concatenate.
        let scope = Scope::with_args(
            vec![
                ("a".to_string(), Value::string("foo bar")),
                ("b".to_string(), Value::string("baz qux")),
            ],
            vec![],
        );
        assert_eq!(eval_ok("a + b", &scope), Value::string("foo barbaz qux"));
        let scope = Scope::with_args(
            vec![
                ("s".to_string(), Value::string("n=")),
                ("f".to_string(), Value::float(2.0)),
            ],
            vec![],
        );
        assert_eq!(eval_ok("s + 5", &scope), Value::string("n=5"));
        assert_eq!(eval_ok("5 + s", &scope), Value::string("5n="));
        assert_eq!(eval_ok("s + f", &scope), Value::string("n=2.0"));
        assert_eq!(eval_ok("f + s", &scope), Value::string("2.0n="));
    }

    #[test]
    fn add_mixed_operands_are_e011() {
        let scope = Scope::with_args(
            vec![
                ("flag".to_string(), Value::bool(true)),
                ("nothing".to_string(), Value::null()),
                ("s".to_string(), Value::string("x y")),
            ],
            vec![],
        );
        for src in [
            "flag + 1",
            "1 + flag",
            "nothing + 1",
            "s + flag",
            "flag + s",
        ] {
            assert_eq!(eval_err(src, &scope).code, E011, "{src}");
        }
    }

    #[test]
    fn numeric_ops_require_numbers() {
        let scope = Scope::with_args(
            vec![("s".to_string(), Value::string("hello world"))],
            vec![],
        );
        for src in [
            "s - 1", "s * 2", "s / 2", "s % 2", "s ** 2", "1 - s", "2 * s", "2 / s", "2 % s",
            "2 ** s", "-s",
        ] {
            assert_eq!(eval_err(src, &scope).code, E011, "{src}");
        }
    }

    #[test]
    fn unary_minus() {
        let empty = Scope::new();
        assert_eq!(eval_ok("-5", &empty), Value::int(-5));
        assert_eq!(eval_ok("--5", &empty), Value::int(5));
        assert_eq!(eval_ok("-5.5", &empty), Value::float(-5.5));
        assert_eq!(eval_ok("-(1 + 2)", &empty), Value::int(-3));
        assert_eq!(eval_ok("-2 * 3", &empty), Value::int(-6));
        assert_eq!(eval_ok("-2 + 3", &empty), Value::int(1));
        let scope = Scope::with_args(vec![("x".to_string(), Value::int(5))], vec![]);
        assert_eq!(eval_ok("-x", &scope), Value::int(-5));
    }

    #[test]
    fn dollar_prefix_requires_digits() {
        let empty = Scope::new();
        for src in ["$x", "$x + 1", "$ + 1", "$ 1", "$?"] {
            let d = eval_err(src, &empty);
            assert_eq!(d.code, E010, "{src}");
            assert!(d.message.contains('$'), "{src}: {}", d.message);
        }
    }

    #[test]
    fn missing_arguments_are_e003() {
        let empty = Scope::new();
        let d = eval_err("y + 1", &empty);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("y"), "{}", d.message);
        let d = eval_err("$5", &empty);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("$5"), "{}", d.message);
    }

    #[test]
    fn bare_identifier_resolves_named_arg_or_last() {
        let scope = Scope::reduce_step(
            vec![("x".to_string(), Value::int(2))],
            vec![],
            Value::int(5),
        );
        assert_eq!(eval_ok("x + last", &scope), Value::int(7));
        assert_eq!(eval_err("last", &Scope::new()).code, E003);
    }

    // ---- String re-scan (operand resolution) ----

    #[test]
    fn string_operands_are_rescanned_as_math() {
        let scope = Scope::reduce_step(vec![], vec![], Value::string("1 + 2"));
        assert_eq!(eval_ok("last", &scope), Value::int(3));

        let scope = Scope::with_args(vec![("n".to_string(), Value::string("123"))], vec![]);
        assert_eq!(eval_ok("n", &scope), Value::int(123));

        let scope = Scope::with_args(
            vec![
                ("s".to_string(), Value::string("x + 1")),
                ("x".to_string(), Value::int(41)),
            ],
            vec![],
        );
        assert_eq!(eval_ok("s", &scope), Value::int(42));

        let scope = Scope::with_args(vec![("x".to_string(), Value::string("5"))], vec![]);
        assert_eq!(eval_ok("x + 1", &scope), Value::int(6));

        let scope = Scope::reduce_step(vec![], vec![], Value::string("1 + 2"));
        assert_eq!(eval_ok("last * 2", &scope), Value::int(6));

        let scope = Scope::with_args(vec![], vec![Value::string("2 * 3")]);
        assert_eq!(eval_ok("$0 + 1", &scope), Value::int(7));
    }

    #[test]
    fn non_parseable_strings_stay_string_operands() {
        let scope = Scope::with_args(
            vec![
                ("a".to_string(), Value::string("free text")),
                ("b".to_string(), Value::string("two words")),
                ("s".to_string(), Value::string("n=")),
            ],
            vec![],
        );
        assert_eq!(eval_ok("a", &scope), Value::string("free text"));
        assert_eq!(
            eval_ok("a + b", &scope),
            Value::string("free texttwo words")
        );
        assert_eq!(eval_ok("s + a", &scope), Value::string("n=free text"));
        assert_eq!(eval_err("a * 2", &scope).code, E011);
        assert_eq!(eval_err("a - 1", &scope).code, E011);
        assert_eq!(eval_err("-a", &scope).code, E011);
    }

    #[test]
    fn rescanned_string_errors_propagate() {
        let scope = Scope::with_args(vec![("x".to_string(), Value::string("y"))], vec![]);
        let d = eval_err("x", &scope);
        assert_eq!(d.code, E003, "gotcha: re-scan resolves the identifier `y`");
        assert!(d.message.contains('y'), "{}", d.message);

        let scope = Scope::with_args(vec![("x".to_string(), Value::string("5 / 0"))], vec![]);
        let d = eval_err("x", &scope);
        assert_eq!(d.code, E011, "semantic errors from the re-scan propagate");
        assert!(d.message.contains("zero"), "{}", d.message);
    }

    #[test]
    fn rescanned_string_uses_current_scope_arguments() {
        let scope = Scope::with_args(
            vec![
                ("expr".to_string(), Value::string("x + y")),
                ("x".to_string(), Value::int(2)),
                ("y".to_string(), Value::int(40)),
            ],
            vec![],
        );
        assert_eq!(eval_ok("expr", &scope), Value::int(42));
    }

    #[test]
    fn self_referential_string_hits_depth_limit() {
        let scope = Scope::with_args(vec![("x".to_string(), Value::string("x"))], vec![]);
        let d = eval_err("x", &scope);
        assert_eq!(d.code, E008);
        assert!(d.message.contains("max-depth"), "{}", d.message);
    }

    #[test]
    fn junk_tokens_are_e010() {
        let empty = Scope::new();
        for src in [
            "1 < 2", "1 > 2", "1 = 2", "1 == 2", "1 <= 2", "\"x\"", "'x'", "1 \\ 2", "1 and 2",
            "1 or 2", "1 & 2", "1 | 2", "1 ! 2", "a.b", "a.5",
        ] {
            assert_eq!(eval_err(src, &empty).code, E010, "{src}");
        }
    }

    #[test]
    fn dangling_or_trailing_syntax_is_e010() {
        let empty = Scope::new();
        for src in [
            "", "1 +", "1 *", "(1", "1)", "()", "1 2", "b(1,)", "b(1 2)", "b(", "1e", "5.",
        ] {
            assert_eq!(eval_err(src, &empty).code, E010, "{src}");
        }
    }

    #[test]
    fn whitespace_is_allowed() {
        let empty = Scope::new();
        assert_eq!(eval_ok(" 1 + 2 ", &empty), Value::int(3));
        assert_eq!(eval_ok("1\t+\n2", &empty), Value::int(3));
        assert_eq!(eval_ok(" 5 / 2 ", &empty), Value::float(2.5));
    }

    #[test]
    fn number_literals_int_and_float() {
        let empty = Scope::new();
        assert_eq!(eval_ok("123", &empty), Value::int(123));
        assert_eq!(eval_ok("1.5", &empty), Value::float(1.5));
        assert_eq!(eval_ok("0.1", &empty), Value::float(0.1));
        assert_eq!(eval_ok("1e2", &empty), Value::float(100.0));
        assert_eq!(eval_ok("2.5e1", &empty), Value::float(25.0));
        assert_eq!(
            eval_ok("99999999999999999999999", &empty),
            Value::float(1e23)
        );
    }

    #[test]
    fn diagnostics_carry_scope_context_and_spans() {
        let mut scope = Scope::new();
        scope.file = Some(PathBuf::from("/proj/main.yml"));
        scope.component = Some("main".to_string());
        scope.span = Span { line: 5, col: 10 };
        let d = eval_err("1 < 2", &scope);
        assert_eq!(d.code, E010);
        assert_eq!(d.file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert_eq!(d.component.as_deref(), Some("main"));
        assert_eq!((d.line, d.col), (5, 12));
        let d = eval_err("1\n< 2", &scope);
        assert_eq!((d.line, d.col), (6, 1));
        let d = eval_err("y + 1", &scope);
        assert_eq!(d.code, E003);
        assert_eq!((d.line, d.col), (5, 10));
    }
}
