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

use ymx_core::diag::{Diagnostic, FileId, Span};
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

/// One concrete `_test` case: the namespace-qualified target component name,
/// the call arguments, the expected outcome, and the anchor of the `_test`
/// block that declared it (its [`FileId`] and a 1:1 span — raw meta values are
/// span-less, mirroring the `ymx-config` convention).
#[derive(Debug, Clone, PartialEq)]
pub struct Test {
    /// Namespace-qualified target component name (`main`, `subdir.comp`).
    pub target: String,
    /// The call arguments for the target (mirrors the call-site grammar).
    pub args: TestArgs,
    /// What the test asserts about the target's compilation.
    pub expected: Expected,
    /// The `_test` block's host document.
    pub file: FileId,
    /// Anchor of the `_test` block (always 1:1 — raw meta values carry no
    /// spans).
    pub span: Span,
}

/// The outcome of running one [`Test`]: the actual compile result plus the
/// pass/fail verdict against the test's expectation.
#[derive(Debug, Clone)]
pub struct TestResult {
    /// The test that was run.
    pub test: Test,
    /// The target's actual compile result: `Ok(value)` or the collected
    /// diagnostics.
    pub actual: Result<Value, Vec<Diagnostic>>,
    /// `true` iff the actual outcome meets the expectation (`Expected::Value`
    /// requires an equal compiled value; `Expected::Error` requires a
    /// diagnostic with the expected code).
    pub passed: bool,
}