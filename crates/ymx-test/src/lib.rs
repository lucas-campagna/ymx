//! `_test` meta-key logic (milestone 1.9).
//!
//! [`parse_tests`] turns the raw `_test` meta values collected on an
//! already-loaded [`Project`] into concrete [`Test`]s (shapes A, B-value,
//! B-error, type-2 map, list-of-type-2), enforcing same-document targeting
//! (`E002`) and the B-mapping invariants (missing/both `result`+`error`,
//! non-string `error` -> `E010`). [`run_tests`] compiles each target via
//! [`compile_component`](ymx_core::resolve::compile_component) and compares
//! against [`Expected::Value`] or matches a diagnostic code for
//! [`Expected::Error`] (only post-load codes are reachable — the project is
//! already loaded, invariant #2).
//!
//! `ymx-test` is I/O-free: it consumes an already-loaded [`Project`] and
//! never touches the filesystem (that is `ymx-lib`'s job).

use ymx_core::ir::Value;

/// The per-test call arguments, mirroring the call-site grammar (rule 3):
/// named (mapping), positional (list, binding `$0`, `$1`, …), or a scalar
/// (binds `$0`). `None` = no arguments.
///
/// `args` values are taken **literally** as YAML values — they are not
/// interpolated at `_test`-parse time; the target component resolves any
/// `$name` / `${...}` / `$call(...)` inside them against the arguments the
/// test binds it.
#[derive(Debug, Clone, PartialEq)]
pub enum TestArgs {
    /// No arguments.
    None,
    /// A mapping — named arguments `(name, value)` in insertion order.
    Named(Vec<(String, Value)>),
    /// A list — positional arguments binding `$0`, `$1`, ….
    Positional(Vec<Value>),
    /// A scalar — binds `$0`.
    Scalar(Value),
}

/// What a test asserts about its target.
#[derive(Debug, Clone, PartialEq)]
pub enum Expected {
    /// The target must compile to this value.
    Value(Value),
    /// The target must produce a diagnostic with the given code (e.g. `"E002"`).
    Error { code: String },
}