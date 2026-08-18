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
use ymx_core::ir::{Args, Value};
use ymx_core::project::{Options, Project};
use ymx_core::resolve::{compile_component, resolve_entry};

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
/// `CliOverrides::default_for_tests()`, so nothing in 1.9 observes it). If
/// the entry file cannot be located the parse fails (`E009`); if the entry
/// component is not defined bare A/B also fail to resolve, but type-2-only
/// `_test` blocks parse successfully without ever consulting the entry component.
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

    // First pass: determine if any test block is bare A or bare B.
    // Bare B = top-level object with `result` or `error` key.
    // Bare A = scalar (Null/Bool/Int/Float/String, never mixed with type-2 in the same doc).
    // A top-level list is always shape 4 (type-2 only).
    let has_bare_ab = project.raw_meta_test.iter().any(|(_, value)| match value {
        Value::Object(m) if m.contains_key("result") || m.contains_key("error") => true,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) => true,
        Value::Object(_) | Value::Array(_) => false,
    });

    // Only resolve the entry when bare A/B shapes are present.
    // Type-2-only projects never consult the entry component.
    let entry_target = if has_bare_ab {
        match resolve_entry(project, "main.main") {
            Ok((_, namespace, component)) => {
                if namespace.is_empty() {
                    component.to_string()
                } else {
                    format!("{namespace}.{component}")
                }
            }
            Err(diag) => return Err(vec![diag]),
        }
    } else {
        // Type-2 only: entry_target is unused but we need a sentinel.
        String::new()
    };

    let mut tests: Vec<Test> = Vec::new();
    let mut diags: Vec<Diagnostic> = Vec::new();
    for (file, value) in &project.raw_meta_test {
        let span = Span { line: 1, col: 1 };
        match value {
            // Bare B: a mapping containing `result` or `error`, targeting the
            // entry component.
            Value::Object(m) if m.contains_key("result") || m.contains_key("error") => {
                debug_assert!(has_bare_ab);
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
            scalar => {
                debug_assert!(has_bare_ab);
                tests.push(Test {
                    target: entry_target.clone(),
                    args: TestArgs::None,
                    expected: Expected::Value(scalar.clone()),
                    file: *file,
                    span,
                });
            }
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
        Some(Value::Object(named)) => {
            TestArgs::Named(named.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        }
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
    // Try exact match first.
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
    // For components, also try appending trailing `$` (top-level `a$` shorthand).
    if !key.ends_with('$') {
        let with_dollar = format!("{}$", key);
        let mut paths: Vec<&str> = project
            .namespaces
            .namespaces()
            .filter(|(_, ns)| {
                ns.get(&with_dollar)
                    .map(|def| def.file == file)
                    .unwrap_or(false)
            })
            .map(|(path, _)| path)
            .collect();
        paths.sort_unstable();
        if let Some(path) = paths.first() {
            return Some(if path.is_empty() {
                with_dollar
            } else {
                format!("{path}.{with_dollar}")
            });
        }
        if project.file_scoped.get(file, &with_dollar).is_some() {
            return Some(with_dollar);
        }
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

/// Runs each parsed test by compiling its target component with `args` under
/// `opts` against an already-loaded [`Project`] (the caller runs
/// `load_project` first; a failed load yields no `Project` and thus no tests
/// run — load-time codes are never matchable).
///
/// Re-parses the raw `_test` blocks internally (the signature has no test
/// list); an internal parse failure returns an empty [`Vec`] — the caller is
/// required to call [`parse_tests`] first and treat its `Err` as fatal, so
/// this path is unreachable in practice. `parse_tests`' diagnostics never
/// participate in `Expected::Error` matching (the caller surfaces them before
/// running), and neither do `extract_options` diagnostics: `opts` arrives
/// already extracted and `run_tests` only ever observes the target's
/// `compile_component` result.
///
/// For [`Expected::Value`], `passed` is true iff `actual` is `Ok(v)` with
/// `v == expected`. For [`Expected::Error`], `passed` is true iff some
/// diagnostic from the target's compilation has `code == expected.code`
/// (post-load codes only).
pub fn run_tests(project: &Project, opts: &Options) -> Vec<TestResult> {
    let tests = match parse_tests(project) {
        Ok(tests) => tests,
        Err(_) => return Vec::new(),
    };
    tests
        .into_iter()
        .map(|test| {
            let actual = compile_component(project, &test.target, &test_args(&test.args), opts);
            let passed = match &test.expected {
                Expected::Value(expected) => match &actual {
                    Ok(value) => value == expected,
                    Err(_) => false,
                },
                Expected::Error { code } => match &actual {
                    Err(diags) => diags.iter().any(|d| d.code == code.as_str()),
                    Ok(_) => false,
                },
            };
            TestResult {
                test,
                actual,
                passed,
            }
        })
        .collect()
}

/// Map a test's [`TestArgs`] to the resolver's [`Args`]: `None` -> `Args::None`,
/// named -> `Args::Named`, positional -> `Args::Positional`, scalar -> a
/// single-element positional list (a scalar binds `$0`).
fn test_args(args: &TestArgs) -> Args {
    match args {
        TestArgs::None => Args::None,
        TestArgs::Named(named) => Args::Named(named.clone()),
        TestArgs::Positional(positional) => Args::Positional(positional.clone()),
        TestArgs::Scalar(scalar) => Args::Positional(vec![scalar.clone()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use ymx_core::diag::{E001, E002, E009, E010};
    use ymx_core::namespace::Definition;
    use ymx_core::parse::{node_to_value, parse_document, Node};

    const SPAN: Span = Span { line: 1, col: 1 };

    fn def(file: u32, name: &str) -> Definition {
        Definition {
            file: FileId(file),
            full_name: name.to_string(),
            span: SPAN,
            body: Node::Int(1, SPAN),
            math_shorthand: false,
        }
    }

    fn def_body(file: u32, name: &str, body: Node) -> Definition {
        Definition {
            file: FileId(file),
            full_name: name.to_string(),
            span: SPAN,
            body,
            math_shorthand: false,
        }
    }

    /// Project rooted at `/proj`:
    /// * `main.yml`     (FileId 0): `main`, `result`, `args`, `error`; file-scoped `_x`
    /// * `a/b.yml`      (FileId 1): `x`
    /// * `subdir/t.yml` (FileId 2): `t`, `x`
    fn project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![
            PathBuf::from("/proj/main.yml"),
            PathBuf::from("/proj/a/b.yml"),
            PathBuf::from("/proj/subdir/t.yml"),
        ];
        p.namespaces.register("", def(0, "main")).unwrap();
        p.namespaces.register("", def(0, "$box")).unwrap();
        p.namespaces.register("", def(0, "result")).unwrap();
        p.namespaces.register("", def(0, "args")).unwrap();
        p.namespaces.register("", def(0, "error")).unwrap();
        p.namespaces.register("a", def(1, "x")).unwrap();
        p.namespaces.register("subdir", def(2, "t")).unwrap();
        p.namespaces.register("subdir", def(2, "x")).unwrap();
        p.file_scoped.register(FileId(0), def(0, "_x")).unwrap();
        p
    }

    /// Raw span-less value of inline YAML (mirrors ymx-config's `value_of`).
    fn value_of(src: &str) -> Value {
        node_to_value(&parse_document(src).expect("parse inline yaml"))
    }

    /// Attach a raw `_test` value to document `file`.
    fn with_test(mut p: Project, file: u32, src: &str) -> Project {
        p.raw_meta_test.push((FileId(file), value_of(src)));
        p
    }

    // ---- malformed `_test` B mappings -> E010 (unreachable-by-construction
    // diagnostics, exercised via crate #[test]) ----

    #[test]
    fn b_mapping_missing_both_result_and_error_is_e010() {
        // The top-level / type-2 disambiguation only classifies a mapping as a
        // B when it contains `result` or `error`, so a both-missing B is
        // unreachable through parse_tests; the invariant is enforced
        // defensively and exercised here against the B validator directly.
        let p = project();
        let v = value_of("args: [1]\n");
        let diag = b_check(&p, FileId(0), Some("main"), &v).unwrap_err();
        assert_eq!(diag.code, E010);
        assert_eq!(diag.file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert_eq!((diag.line, diag.col), (1, 1));
        assert_eq!(diag.component.as_deref(), Some("main"));
        assert!(diag.message.contains("result"), "{}", diag.message);
    }

    #[test]
    fn type2_b_with_both_result_and_error_is_e010() {
        let p = with_test(project(), 0, "main:\n  result: 1\n  error: \"E002\"\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.code, E010);
        assert_eq!(d.file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert_eq!((d.line, d.col), (1, 1));
        assert_eq!(d.component.as_deref(), Some("main"));
        assert!(d.message.contains("mutually exclusive"), "{}", d.message);
    }

    #[test]
    fn bare_b_with_both_result_and_error_is_e010() {
        let p = with_test(project(), 0, "result: 1\nerror: \"E002\"\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(
            diags[0].component, None,
            "a bare B is not keyed by a target"
        );
    }

    #[test]
    fn type2_b_with_non_string_error_is_e010() {
        for src in ["main:\n  error: 5\n", "main:\n  error: [\"E002\"]\n"] {
            let p = with_test(project(), 0, src);
            let diags = parse_tests(&p).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("main"), "{src}");
            assert!(
                diags[0].message.contains("string"),
                "{src}: {}",
                diags[0].message
            );
        }
    }

    #[test]
    fn bare_b_with_non_string_error_is_e010() {
        let p = with_test(project(), 0, "error: 5\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component, None);
    }

    #[test]
    fn list_element_that_is_not_a_mapping_is_e010() {
        // A list element can never be bare A/B (same anchoring), so a scalar
        // element is malformed; the other element still parses.
        let p = with_test(project(), 0, "- main: 1\n- 2\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component, None);
    }

    // ---- host-doc E001 (unreachable-by-construction: an un-parseable
    // carrier document never reaches parse_tests) ----

    #[test]
    fn host_doc_parse_error_is_e001() {
        let err = parse_document("---\na: 1\n---\nb: 2\n").unwrap_err();
        let d = err.into_diagnostic(PathBuf::from("/proj/main.yml"));
        assert_eq!(d.code, E001);
        assert_eq!(d.file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert!(d.message.contains("multi-document"), "{}", d.message);
    }

    // ---- shapes ----

    #[test]
    fn bare_a_targets_the_entry_with_no_args() {
        let p = with_test(project(), 0, "42\n");
        let tests = parse_tests(&p).expect("bare A");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].target, "main");
        assert_eq!(tests[0].args, TestArgs::None);
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(42)));
        assert_eq!(tests[0].file, FileId(0));
        assert_eq!(tests[0].span, SPAN);
    }

    #[test]
    fn bare_b_value_targets_the_entry() {
        let p = with_test(project(), 0, "result: 5\n");
        let tests = parse_tests(&p).expect("bare B value");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].target, "main");
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(5)));

        let p = with_test(project(), 0, "args: [1]\nresult: 5\n");
        let tests = parse_tests(&p).expect("bare B value with args");
        assert_eq!(tests[0].args, TestArgs::Positional(vec![Value::Int(1)]));
    }

    #[test]
    fn bare_b_error_targets_the_entry() {
        let p = with_test(project(), 0, "error: \"E002\"\n");
        let tests = parse_tests(&p).expect("bare B error");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].target, "main");
        assert_eq!(
            tests[0].expected,
            Expected::Error {
                code: "E002".to_string()
            }
        );
    }

    #[test]
    fn top_level_result_mapping_is_bare_b_not_type2() {
        // `result:` at the top level is a bare B targeting the entry — the
        // list-wrapping escape is what redirects it to a component named
        // `result`.
        let p = with_test(project(), 0, "result: 1\n");
        let tests = parse_tests(&p).expect("bare B");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].target, "main");
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(1)));
    }

    #[test]
    fn type2_map_value_and_error_variants() {
        let p = with_test(project(), 0, "main: 1\n$box: {args: [2], result: 3}\n");
        let tests = parse_tests(&p).expect("type-2 map");
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].target, "main");
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(1)));
        assert_eq!(tests[1].target, "$box");
        assert_eq!(tests[1].args, TestArgs::Positional(vec![Value::Int(2)]));
        assert_eq!(tests[1].expected, Expected::Value(Value::Int(3)));

        let p = with_test(project(), 0, "main: {error: \"E002\"}\n");
        let tests = parse_tests(&p).expect("type-2 error variant");
        assert_eq!(tests.len(), 1);
        assert_eq!(
            tests[0].expected,
            Expected::Error {
                code: "E002".to_string()
            }
        );
    }

    #[test]
    fn type2_mapping_value_without_result_or_error_is_a_literal_a() {
        // A mapping value without `result`/`error` is an A: the expected value
        // is the mapping itself.
        let p = with_test(project(), 0, "main: {a: 1}\n");
        let tests = parse_tests(&p).expect("type-2 A");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].expected, Expected::Value(value_of("a: 1\n")));
    }

    #[test]
    fn type2_sub_namespace_targets_emit_dotted_form() {
        // The `_test` block lives in subdir/t.yml (FileId 2), so its same-doc
        // targets `t`/`x` emit the dotted `subdir.t`/`subdir.x` forms.
        let p = with_test(project(), 2, "t: 1\nx: {args: 2, result: 3}\n");
        let tests = parse_tests(&p).expect("subdir type-2 map");
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].target, "subdir.t");
        assert_eq!(tests[1].target, "subdir.x");
        assert_eq!(tests[1].args, TestArgs::Scalar(Value::Int(2)));
    }

    #[test]
    fn file_scoped_underscore_target_resolves_bare() {
        let p = with_test(project(), 0, "_x: 1\n");
        let tests = parse_tests(&p).expect("file-scoped target");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].target, "_x");
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(1)));
    }

    #[test]
    fn list_of_type2_maps() {
        let p = with_test(project(), 0, "- main: 1\n- main: 2\n");
        let tests = parse_tests(&p).expect("list of type-2 maps");
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].target, "main");
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(1)));
        assert_eq!(tests[1].expected, Expected::Value(Value::Int(2)));
    }

    #[test]
    fn list_wrapping_escape_targets_named_result_args_error() {
        // Wrapped in a list, `result`/`args`/`error` are type-2 target names;
        // unwrapped they would be bare-B keys targeting the entry.
        let p = with_test(project(), 0, "- result: 1\n- args: 2\n- error: 3\n");
        let tests = parse_tests(&p).expect("list-wrapping escape");
        assert_eq!(tests.len(), 3);
        assert_eq!(tests[0].target, "result");
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(1)));
        assert_eq!(tests[1].target, "args");
        assert_eq!(tests[2].target, "error");
        assert_eq!(tests[2].expected, Expected::Value(Value::Int(3)));
    }

    #[test]
    fn b_args_forms_map_to_test_args() {
        let cases = [
            (
                "main: {args: {a: 1, b: 2}, result: 3}\n",
                TestArgs::Named(vec![
                    ("a".to_string(), Value::Int(1)),
                    ("b".to_string(), Value::Int(2)),
                ]),
            ),
            (
                "main: {args: [1, 2], result: 3}\n",
                TestArgs::Positional(vec![Value::Int(1), Value::Int(2)]),
            ),
            (
                "main: {args: 7, result: 8}\n",
                TestArgs::Scalar(Value::Int(7)),
            ),
            ("main: {result: 1}\n", TestArgs::None),
        ];
        for (src, expected_args) in cases {
            let p = with_test(project(), 0, src);
            let tests = parse_tests(&p).expect(src);
            assert_eq!(tests.len(), 1, "{src}");
            assert_eq!(tests[0].args, expected_args, "{src}");
        }
    }

    #[test]
    fn extra_keys_in_a_b_mapping_are_ignored() {
        let p = with_test(project(), 0, "main: {result: 1, extra: 2}\n");
        let tests = parse_tests(&p).expect("extra keys ignored");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].expected, Expected::Value(Value::Int(1)));
    }

    // ---- same-document targeting ----

    #[test]
    fn cross_document_target_is_e002() {
        // `x` is defined in a/b.yml and subdir/t.yml, not in main.yml, which
        // hosts the `_test` block; `t` is likewise foreign. All misses are
        // collected (no short-circuit).
        let p = with_test(project(), 0, "x: 1\nt: 2\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 2);
        for d in &diags {
            assert_eq!(d.code, E002);
            assert_eq!(d.file.as_deref(), Some(Path::new("/proj/main.yml")));
            assert_eq!((d.line, d.col), (1, 1));
        }
        let components: Vec<&str> = diags
            .iter()
            .map(|d| d.component.as_deref().unwrap())
            .collect();
        assert_eq!(components, ["x", "t"]);
    }

    #[test]
    fn unknown_target_with_malformed_b_reports_both() {
        // `nope` is unknown (E002) and its B is malformed (both result+error,
        // E010) — independent errors, both collected.
        let p = with_test(project(), 0, "nope:\n  result: 1\n  error: \"E002\"\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().any(|d| d.code == E002));
        assert!(diags.iter().any(|d| d.code == E010));
    }

    #[test]
    fn no_test_blocks_parse_to_empty_without_entry() {
        let p = project();
        let tests = parse_tests(&p).expect("no _test blocks");
        assert!(tests.is_empty());

        // No tests and no entry file: the entry is never resolved.
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![PathBuf::from("/proj/a/b.yml")];
        p.namespaces.register("a", def(1, "x")).unwrap();
        assert!(parse_tests(&p).expect("no tests").is_empty());
    }

    #[test]
    fn unresolvable_default_entry_is_e009() {
        // No main.yml anywhere, yet a `_test` block exists: bare A/B cannot
        // produce a target, so the default-entry E009 propagates.
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![PathBuf::from("/proj/a/b.yml")];
        p.namespaces.register("a", def(1, "x")).unwrap();
        let p = with_test(p, 1, "5\n");
        let diags = parse_tests(&p).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E009);
    }

    #[test]
    fn type2_only_scenario_without_main_parses_successfully() {
        // A project with a/b.yml defining `x`, a _test block `{x: {result: 7}}`,
        // and NO main.yml at all — parse_tests returns Ok because type-2 maps
        // never consult the entry component.
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![PathBuf::from("/proj/a/b.yml")];
        p.namespaces.register("a", def(1, "x")).unwrap();
        let p = with_test(p, 1, "x: {result: 7}\n");
        let tests = parse_tests(&p).expect("type-2 only, no main");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].target, "a.x");
        assert_eq!(tests[0].args, TestArgs::None);
        assert!(matches!(&tests[0].expected, Expected::Value(Value::Int(7))));
    }

    // ---- run_tests ----

    #[test]
    fn run_tests_value_match_and_mismatch() {
        let p = with_test(project(), 0, "main: 1\n");
        let results = run_tests(&p, &Options::default());
        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
        assert!(
            matches!(&results[0].actual, Ok(v) if v == &Value::Int(1)),
            "actual = Ok for value tests"
        );

        let p = with_test(project(), 0, "main: 2\n");
        let results = run_tests(&p, &Options::default());
        assert!(!results[0].passed);
        assert!(matches!(&results[0].actual, Ok(v) if v == &Value::Int(1)));
    }

    #[test]
    fn run_tests_error_code_match_and_mismatch() {
        let p = with_test(nope_project(), 0, "main: {error: \"E002\"}\n");
        let results = run_tests(&p, &Options::default());
        assert_eq!(results.len(), 1);
        assert!(results[0].passed, "E002 matches the compile diagnostic");
        assert!(results[0].actual.is_err());

        let p = with_test(nope_project(), 0, "main: {error: \"E008\"}\n");
        let results = run_tests(&p, &Options::default());
        assert!(!results[0].passed, "E008 does not match");
        assert!(results[0].actual.is_err());
    }

    /// `main.yml` defining `main` whose body calls the unknown component
    /// `nope` — a compile-time `E002` on every call.
    fn nope_project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![PathBuf::from("/proj/main.yml")];
        p.namespaces
            .register(
                "",
                def_body(0, "main", Node::String("$nope(1)".to_string(), SPAN)),
            )
            .unwrap();
        p
    }

    #[test]
    fn run_tests_args_are_literal_and_bind_into_the_target() {
        // `$x` inside `args` is a literal string; `main`'s body `$a` resolves
        // it against the bound argument, so the result is the literal `$x`.
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![PathBuf::from("/proj/main.yml")];
        p.namespaces
            .register(
                "",
                def_body(0, "main", Node::String("$a".to_string(), SPAN)),
            )
            .unwrap();
        let p = with_test(p, 0, "main: {args: {a: \"$x\"}, result: \"$x\"}\n");
        let results = run_tests(&p, &Options::default());
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].test.args,
            TestArgs::Named(vec![("a".to_string(), Value::String("$x".to_string()))])
        );
        assert!(results[0].passed);

        // A scalar binds `$0`; the literal value flows into the target.
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![PathBuf::from("/proj/main.yml")];
        p.namespaces
            .register(
                "",
                def_body(0, "main", Node::String("$0".to_string(), SPAN)),
            )
            .unwrap();
        let p = with_test(p, 0, "main: {args: 5, result: 5}\n");
        let results = run_tests(&p, &Options::default());
        assert!(results[0].passed);
    }

    #[test]
    fn run_tests_internal_parse_error_returns_empty() {
        // The caller is required to surface parse_tests' Err first; run_tests
        // re-parses internally and degrades to no results on failure.
        let p = with_test(project(), 0, "main:\n  result: 1\n  error: \"E002\"\n");
        assert!(parse_tests(&p).is_err());
        assert!(run_tests(&p, &Options::default()).is_empty());
    }
}
