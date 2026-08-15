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

use ymx_core::diag::{Diagnostic, FileId, Span, E002, E010};
use ymx_core::ir::Value;
use ymx_core::project::Project;
use ymx_core::resolve::resolve_entry;

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

/// Parses the raw `_test` meta blocks of an already-loaded [`Project`] into
/// concrete [`Test`]s (same-document targeting).
///
/// Top-level shapes (PRD `_test`): bare A (scalar -> entry, no args), bare B
/// (mapping with a `result` or `error` key -> entry), type-2 map (a mapping
/// without `result`/`error`, each key a component of the same document), and
/// list-of-type-2 (a list is always shape 4; each element a type-2 map). A
/// type-2 value is a B if it is a mapping containing `result`/`error`, else an
/// A (literal expected value).
///
/// Bare A/B target the project entry — resolved via
/// [`resolve_entry`](ymx_core::resolve::resolve_entry) against the literal
/// default entry path `main.main` (this crate has no [`Options`], so the
/// `--entry` override is invisible here; the harness uses
/// `CliOverrides::default_for_tests()`, so nothing in 1.9 observes it). An
/// unresolvable default entry (`E009`) propagates as the sole parse error: no
/// test targets can be produced without a resolvable entry.
///
/// Same-document targeting: every type-2 key must resolve to a definition
/// hosted by the `_test` block's own document — namespace defs (global +
/// sub-namespaces) emit the bare key or the dotted `{namespace}.{key}` form;
/// file-scoped `_`-prefixed keys emit the bare key. A miss is `E002`
/// (collected, no short-circuit). A B mapping containing neither `result` nor
/// `error`, both, or a non-string `error` value is a malformed `_test` block
/// (`E010`), as is a top-level-list element that is not a mapping (a list
/// element can never be bare A/B). `args` is optional (absent = no arguments)
/// and mirrors the call-site grammar: mapping -> named, list -> positional,
/// scalar -> binds `$0`; `args` values are taken literally (no interpolation
/// at parse time).
///
/// Tests are produced in load order (lexicographic document order, then
/// insertion order within a block); a test's `file`/`span` anchor the `_test`
/// block at 1:1 (raw meta values are span-less).
pub fn parse_tests(project: &Project) -> Result<Vec<Test>, Vec<Diagnostic>> {
    if project.raw_meta_test.is_empty() {
        return Ok(Vec::new());
    }
    let entry_target = match resolve_entry(project, "main.main") {
        Ok((_, namespace, component)) => {
            if namespace.is_empty() {
                component.to_string()
            } else {
                format!("{namespace}.{component}")
            }
        }
        Err(diag) => return Err(vec![diag]),
    };

    let mut tests: Vec<Test> = Vec::new();
    let mut diags: Vec<Diagnostic> = Vec::new();
    for (file, value) in &project.raw_meta_test {
        let span = Span { line: 1, col: 1 };
        match value {
            // Bare B: a mapping containing `result` or `error`, targeting the
            // entry component.
            Value::Object(m) if m.contains_key("result") || m.contains_key("error") => {
                match parse_b(project, *file, span, value, &entry_target, None) {
                    Ok(test) => tests.push(test),
                    Err(diag) => diags.push(diag),
                }
            }
            // Type-2 map: a mapping without `result`/`error`.
            Value::Object(_) => parse_type2(project, *file, span, value, &mut tests, &mut diags),
            // List of type-2 maps: a top-level list is always shape 4.
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(_) = item {
                        parse_type2(project, *file, span, item, &mut tests, &mut diags);
                    } else {
                        diags.push(Diagnostic {
                            file: Some(project.files[file.0 as usize].clone()),
                            line: 1,
                            col: 1,
                            component: None,
                            code: E010,
                            message:
                                "malformed `_test` block: a list element must be a type-2 mapping (bare A/B are not allowed inside a list)"
                                    .to_string(),
                        });
                    }
                }
            }
            // Bare A: a scalar targeting the entry component with no args.
            scalar => tests.push(Test {
                target: entry_target.clone(),
                args: TestArgs::None,
                expected: Expected::Value(scalar.clone()),
                file: *file,
                span,
            }),
        }
    }
    if diags.is_empty() {
        Ok(tests)
    } else {
        Err(diags)
    }
}

/// Parse one type-2 map (shape 3, or a shape-4 list element) into per-key
/// tests. Each key must be defined in the same document as the `_test` block
/// (`E002` on a miss, collected); a value that is a mapping containing
/// `result`/`error` is a B for that component, anything else is an A (literal
/// expected value).
fn parse_type2(
    project: &Project,
    file: FileId,
    span: Span,
    value: &Value,
    tests: &mut Vec<Test>,
    diags: &mut Vec<Diagnostic>,
) {
    let Value::Object(m) = value else {
        unreachable!("a type-2 map is always an object");
    };
    for (key, item) in m {
        let target = same_doc_target(project, file, key);
        let Some(target) = target else {
            diags.push(same_doc_miss(project, file, key));
            // A malformed B is still reported even when the target is unknown
            // (independent errors; no short-circuit).
            if let Value::Object(bm) = item {
                if bm.contains_key("result") || bm.contains_key("error") {
                    if let Err(diag) = b_check(project, file, Some(key), item) {
                        diags.push(diag);
                    }
                }
            }
            continue;
        };
        match item {
            Value::Object(bm) if bm.contains_key("result") || bm.contains_key("error") => {
                match parse_b(project, file, span, item, &target, Some(key)) {
                    Ok(test) => tests.push(test),
                    Err(diag) => diags.push(diag),
                }
            }
            _ => tests.push(Test {
                target,
                args: TestArgs::None,
                expected: Expected::Value(item.clone()),
                file,
                span,
            }),
        }
    }
}

/// Parse one B mapping (value variant `{args,result}` / error variant
/// `{args,error}`) into a [`Test`]. `component` is the type-2 key for the
/// `E010` anchor (`None` for a bare B).
fn parse_b(
    project: &Project,
    file: FileId,
    span: Span,
    value: &Value,
    target: &str,
    component: Option<&str>,
) -> Result<Test, Diagnostic> {
    let Value::Object(m) = value else {
        unreachable!("a B mapping is always an object");
    };
    b_check(project, file, component, value)?;
    let args = match m.get("args") {
        None => TestArgs::None,
        Some(Value::Object(named)) => TestArgs::Named(
            named
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        Some(Value::Array(positional)) => TestArgs::Positional(positional.clone()),
        Some(scalar) => TestArgs::Scalar(scalar.clone()),
    };
    let expected = if m.contains_key("error") {
        match &m["error"] {
            Value::String(code) => Expected::Error { code: code.clone() },
            _ => unreachable!("b_check ensured `error` is a string"),
        }
    } else {
        Expected::Value(m["result"].clone())
    };
    Ok(Test {
        target: target.to_string(),
        args,
        expected,
        file,
        span,
    })
}

/// The B-mapping invariants (PRD): a B mapping must contain exactly one of
/// `result` (value variant) / `error` (error variant), and `error` must be a
/// string code. A violation is one `E010` diagnostic.
fn b_check(
    project: &Project,
    file: FileId,
    component: Option<&str>,
    value: &Value,
) -> Result<(), Diagnostic> {
    let Value::Object(m) = value else {
        unreachable!("a B mapping is always an object");
    };
    let malformed = |detail: &str| Diagnostic {
        file: Some(project.files[file.0 as usize].clone()),
        line: 1,
        col: 1,
        component: component.map(str::to_string),
        code: E010,
        message: format!("malformed `_test` block: {detail}"),
    };
    match (m.contains_key("result"), m.contains_key("error")) {
        (true, true) => Err(malformed("`result` and `error` are mutually exclusive")),
        (false, false) => Err(malformed("expected `result` or `error` in the B mapping")),
        (false, true) => match &m["error"] {
            Value::String(_) => Ok(()),
            _ => Err(malformed("`error` must be a string diagnostic code")),
        },
        (true, false) => Ok(()),
    }
}

/// Same-document targeting: `key` must resolve to a definition hosted by
/// `file` (the `_test` block's document). Namespace defs (global + sub) are
/// scanned for a host-file match, emitting the bare key for the global
/// namespace and the dotted form `{namespace}.{key}` for a sub-namespace;
/// file-scoped (`_`-prefixed) keys resolve bare via the file-scope store
/// (`compile_component` pins bare `_` names by lowest [`FileId`], which the
/// same-document check makes the host).
fn same_doc_target(project: &Project, file: FileId, key: &str) -> Option<String> {
    let mut paths: Vec<&str> = project
        .namespaces
        .namespaces()
        .filter(|(_, ns)| ns.get(key).map(|def| def.file == file).unwrap_or(false))
        .map(|(path, _)| path)
        .collect();
    paths.sort_unstable();
    if let Some(path) = paths.first() {
        return Some(if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        });
    }
    if project.file_scoped.get(file, key).is_some() {
        return Some(key.to_string());
    }
    None
}

/// The `E002` diagnostic for a type-2 key that is not defined in the `_test`
/// block's own document, anchored at 1:1 of that document.
fn same_doc_miss(project: &Project, file: FileId, key: &str) -> Diagnostic {
    Diagnostic {
        file: Some(project.files[file.0 as usize].clone()),
        line: 1,
        col: 1,
        component: Some(key.to_string()),
        code: E002,
        message: format!(
            "`_test` target `{key}` is not defined in the same document as the `_test` block"
        ),
    }
}