//! Math engine boundary and evaluation scope.
//!
//! [`MathEngine`] is the trait behind `${...}` evaluation — the seam for
//! swapping in a future Lua/Python/JavaScript engine (PRD *Architecture:
//! Math*). [`Scope`] carries everything an evaluation needs: the in-scope
//! named/positional arguments, the reduce-step `last` value, the
//! component-call dispatch hook for math `name(...)` calls, and the
//! diagnostic context. The v1 dynamic evaluator implementing the trait is
//! milestone 1.5 task 3.

use std::path::PathBuf;

use crate::diag::{Diagnostic, Span, E002};
use crate::ir::Value;

/// The math-engine boundary: evaluates the contents of a `${...}` expression.
///
/// `src` is the raw text between the braces; `scope` supplies the in-scope
/// arguments (named + positional), the reduce-step `last` value, and the
/// component-call hook. The trait is the boundary for swapping to a
/// Lua/Python/JavaScript engine in the future (PRD *Architecture: Math*).
pub trait MathEngine {
    fn eval(&self, src: &str, scope: &Scope) -> Result<Value, Diagnostic>;
}

/// Evaluation scope for `${...}` math and string interpolation.
///
/// Carries the named arguments (rule 2), the positional arguments (rule 4),
/// the previous step's result in a reduce (`last`, rules 13/16 — `None`
/// outside a reduce step or on its first step), the component-call dispatch
/// hook for math `name(...)` calls (wired by the resolver in milestone 1.6,
/// including the `E008` depth check at the call boundary), and the diagnostic
/// context (`file`, `component`, and the base `span` math diagnostics are
/// attributed to).
pub struct Scope {
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
    /// Component-call dispatch hook for math `name(...)` calls: receives the
    /// (possibly dotted) name and the positional argument values. `None`
    /// means no hook is registered (`invoke` then reports `E002`).
    pub call: Option<Box<dyn Fn(&str, &[Value]) -> Result<Value, Diagnostic>>>,
}

impl Scope {
    /// Empty scope: no arguments, not in a reduce, no diagnostic context.
    pub fn new() -> Scope {
        Scope {
            file: None,
            component: None,
            span: Span { line: 1, col: 1 },
            named: Vec::new(),
            positional: Vec::new(),
            last: None,
            call: None,
        }
    }

    /// Scope for a plain (non-reduce) call with `named` and `positional`
    /// arguments.
    pub fn with_args(named: Vec<(String, Value)>, positional: Vec<Value>) -> Scope {
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
    pub fn reduce_step(named: Vec<(String, Value)>, positional: Vec<Value>, last: Value) -> Scope {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let scope = Scope {
            call: Some(Box::new(|name: &str, args: &[Value]| {
                let sum: i64 = args
                    .iter()
                    .map(|v| match v {
                        Value::Int(i) => *i,
                        _ => 0,
                    })
                    .sum();
                Ok(Value::int(sum + name.len() as i64))
            })),
            ..Scope::new()
        };
        assert_eq!(
            scope
                .invoke("b", &[Value::int(12), Value::int(34)])
                .unwrap(),
            Value::int(47)
        );
    }
}
