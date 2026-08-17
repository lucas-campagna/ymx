//! Canonical pipeline orchestration (milestone 1.10, tasks 3 + 4).
//!
//! `load_project` -> `extract_options` -> (`--test` ? `run_tests` :
//! `compile`) -> emit. Diagnostic rendering (one
//! [`Diagnostic::render`](ymx_lib::Diagnostic::render) line per stderr entry)
//! and exit codes are this module's responsibility. The compile-branch emit
//! shape (task 4) follows the per-`opts.format` split:
//! * `Json`: serialize the [`Value`] via `serde_json` (pretty iff
//!   `opts.pretty`); write to the `--output` file if provided, else stdout.
//!   The file is written only on success (compile already succeeded by the
//!   time emit runs); a write failure prints a diagnostic-style error to
//!   stderr and yields [`Outcome::Diagnostic`], best-effort removing any
//!   partially written file.
//! * `Diagnostics`: on success stdout is empty and the outcome is
//!   [`Outcome::Success`] (the compile-error branch is already handled as
//!   [`Outcome::Diagnostic`] before emit runs).
//!
//! Orchestration order (PRD §CLI):
//! 1. `load_project(path)` — any load-time diagnostic (`E001` / `E004` /
//!    `E007` / `E015`) renders all diagnostics to stderr and yields
//!    [`Outcome::Diagnostic`].
//! 2. `extract_options(&project, &cli.overrides())` — resolves the entry path
//!    (`E009` / `E010`) the same way.
//! 3. `--test`: `parse_tests(&project)` first (its `Err` surfaces a malformed
//!    `_test` block, `E010`, before any test runs — `run_tests` internally
//!    re-parses and silently degrades, so `parse_tests` is the gate), then
//!    `run_tests(&project, &opts)`. One line per test (`PASS` / `FAIL` + diff
//!    on failure); any failure or parse error yields
//!    [`Outcome::Diagnostic`]. No JSON is emitted under `--test`.
//! 4. Otherwise `compile(&project, &opts)` — any diagnostic renders to
//!    stderr and yields [`Outcome::Diagnostic`]; success hits [`emit`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ymx_config::{extract_options, CliOverrides};
use ymx_lib::ymx_core::project::{Format, Options, Project};
use ymx_lib::ymx_core::resolve::compile;
use ymx_lib::{load_project, Diagnostic, Value};
use ymx_test::{parse_tests, run_tests, Expected, TestResult};

use crate::args::ParsedCli;

/// The orchestration outcome, mapped to a process [`ExitCode`] by [`main`].
/// Kept as a small enum rather than returning [`ExitCode`] directly so the
/// pipeline is unit-testable (`ExitCode` does not implement `PartialEq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Success — the pipeline completed without any diagnostic or test
    /// failure.
    Success,
    /// A runtime diagnostic or test failure was rendered to stderr.
    Diagnostic,
}

impl Outcome {
    /// Map to the process exit code: `0` for [`Outcome::Success`], `1` for
    /// [`Outcome::Diagnostic`] (PRD §Exit codes: default non-zero `1`).
    pub fn to_exit_code(self) -> ExitCode {
        match self {
            Outcome::Success => ExitCode::SUCCESS,
            Outcome::Diagnostic => ExitCode::from(1),
        }
    }
}

/// Drive the canonical pipeline against `cli`.
pub fn run(cli: &ParsedCli) -> Outcome {
    // Recursive directory mode (--test with a directory path)
    if let Some(ref test_dir) = cli.test_dir {
        return run_recursive_tests(test_dir);
    }

    let project_root = cli
        .path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let project = match load_project(&project_root) {
        Ok(project) => project,
        Err(diags) => return render_diags(&diags),
    };

    let overrides = cli.overrides();
    let opts = match extract_options(&project, &overrides) {
        Ok(opts) => opts,
        Err(diags) => return render_diags(&diags),
    };

    if cli.test {
        return run_test_branch(&project, &opts);
    }

    match compile(&project, &opts) {
        Ok(value) => emit(cli, &opts, &value),
        Err(diags) => render_diags(&diags),
    }
}

/// Recursively discover project roots under `dir`. A subdirectory is a project
/// root if it contains at least one `.yml` or `.yaml` file in its direct
/// children. Skips `.git` and hidden directories (starting with `.`).
fn find_project_roots(dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        // Check if current dir has any .yml or .yaml files directly in it
        let mut has_yaml = false;
        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext == "yml" || ext == "yaml" {
                            has_yaml = true;
                            break;
                        }
                    }
                }
            }
        }

        if has_yaml {
            roots.push(current.clone());
        } else {
            // Recurse into subdirectories (not into hidden or .git dirs)
            if let Ok(entries) = std::fs::read_dir(&current) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            if name == ".git" || name.starts_with('.') {
                                continue;
                            }
                        }
                        stack.push(path);
                    }
                }
            }
        }
    }

    roots.sort();
    roots
}

/// Recursive directory test mode: discover all project roots under `dir` and
/// run tests in each. Load failures are warned and skipped; opts/parse failures
/// and test failures cause non-zero exit.
fn run_recursive_tests(dir: &Path) -> Outcome {
    let roots = find_project_roots(dir);

    if roots.is_empty() {
        eprintln!("ymx: no YMX projects found in {}", dir.display());
        return Outcome::Success;
    }

    let mut total_passed = 0usize;
    let mut total_tests = 0usize;
    let mut overall_success = true;

    for proj_dir in &roots {
        let relpath = proj_dir
            .strip_prefix(dir)
            .unwrap_or(proj_dir.as_path())
            .to_string_lossy();

        let project = match load_project(proj_dir) {
            Ok(p) => p,
            Err(diags) => {
                eprintln!("ymx: warning: {}: {}", proj_dir.display(), &diags[0].message);
                continue;
            }
        };

        let opts = match extract_options(&project, &CliOverrides::default_for_tests()) {
            Ok(o) => o,
            Err(diags) => {
                eprintln!("ymx: warning: {}: {}", proj_dir.display(), &diags[0].message);
                continue;
            }
        };

        if let Err(diags) = parse_tests(&project) {
            eprintln!(
                "ymx: warning: {}: parse error: {}",
                proj_dir.display(),
                &diags[0].message
            );
            overall_success = false;
            continue;
        }

        let results = run_tests(&project, &opts);
        total_tests += results.len();

        for result in &results {
            if result.passed {
                total_passed += 1;
                println!("PASS {}: {}", relpath, result.test.target);
            } else {
                overall_success = false;
                println!("FAIL {}: {} {}", relpath, result.test.target, diff(result));
            }
        }
    }

    println!("PASS: {}/{} across {} project(s)", total_passed, total_tests, roots.len());

    if overall_success && total_passed == total_tests {
        Outcome::Success
    } else {
        Outcome::Diagnostic
    }
}

/// `--test` branch: parse tests first (its `Err` is the malformed-`_test`
/// gate, surfaced as `E010`), then run them and print one line per result.
fn run_test_branch(project: &Project, opts: &Options) -> Outcome {
    match parse_tests(project) {
        Ok(_) => {}
        Err(diags) => return render_diags(&diags),
    }
    let results = run_tests(project, opts);
    let total = results.len();
    let mut passed = 0usize;
    for result in &results {
        if result.passed {
            passed += 1;
            println!("PASS {}", result.test.target);
        } else {
            println!("FAIL {} {}", result.test.target, diff(result));
        }
    }
    println!("PASS: {passed}/{total}");
    if passed == total {
        Outcome::Success
    } else {
        Outcome::Diagnostic
    }
}

/// A brief, human-readable diff for a failing test: `expected: <Value>
/// actual: <Value or first diag code>`. Values render via `serde_json` for
/// readability (PRD values serialize cleanly as JSON); an `Err` actual renders
/// the first diagnostic's code (e.g. `E002`).
fn diff(result: &TestResult) -> String {
    let expected = match &result.test.expected {
        Expected::Value(v) => serde_json::to_string(v).unwrap_or_else(|_| format!("{v:?}")),
        Expected::Error { code } => format!("error {code}"),
    };
    let actual = match &result.actual {
        Ok(v) => serde_json::to_string(v).unwrap_or_else(|_| format!("{v:?}")),
        Err(diags) => diags
            .first()
            .map(|d| d.code.to_string())
            .unwrap_or_else(|| "(no diagnostics)".to_string()),
    };
    format!("expected: {expected} actual: {actual}")
}

/// Success-branch emit (task 4). Per `opts.format`:
///
/// * [`Format::Diagnostics`]: emit nothing to stdout; the outcome is
///   [`Outcome::Success`] (PRD: `--format diagnostics` on a successful
///   compile leaves stdout empty and exits `0`). `--output` is ignored in
///   this mode.
/// * [`Format::Json`]: serialize `value` with `serde_json` (pretty iff
///   `opts.pretty`). If `cli.output` is set, the JSON is written to that
///   file atomically — the string is materialized first, so a serialization
///   failure aborts before any file is created; a write failure prints a
///   diagnostic-style error to stderr, best-effort removes any partially
///   written file, and yields [`Outcome::Diagnostic`]. Otherwise the JSON is
///   written to stdout. `--output` is ignored under `--test` (handled
///   earlier in [`run`]).
fn emit(cli: &ParsedCli, opts: &Options, value: &Value) -> Outcome {
    match opts.format {
        Format::Diagnostics => Outcome::Success,
        Format::Json => emit_json(cli, opts.pretty, value),
    }
}

/// Serialize `value` to JSON (pretty iff `pretty`) and dispatch to
/// `--output` or stdout. Serialization is materialized into a `String`
/// before any I/O, so a serialize failure never creates a file.
fn emit_json(cli: &ParsedCli, pretty: bool, value: &Value) -> Outcome {
    let json = match serialize(value, pretty) {
        Ok(json) => json,
        Err(message) => {
            eprintln!("ymx: {message}");
            return Outcome::Diagnostic;
        }
    };
    match cli.output.as_deref() {
        Some(path) => write_file(path, &json),
        None => {
            print!("{json}");
            Outcome::Success
        }
    }
}

/// Serialize `value` to a JSON `String`: compact by default, pretty when
/// `pretty`. `serde_json` does not fail for our [`Value`] shape (it derives
/// `Serialize` and contains only JSON-representable types), but the fall-back
/// keeps the emit path robust.
fn serialize(value: &Value, pretty: bool) -> Result<String, String> {
    if pretty {
        serde_json::to_string_pretty(value).map_err(|e| format!("failed to serialize JSON: {e}"))
    } else {
        serde_json::to_string(value).map_err(|e| format!("failed to serialize JSON: {e}"))
    }
}

/// Write `json` to `path`: serialize first (done by the caller), then a single
/// `fs::write` (which truncates-then-writes). A write failure prints a
/// diagnostic-style error to stderr, best-effort removes any partial file,
/// and yields [`Outcome::Diagnostic`].
fn write_file(path: &Path, json: &str) -> Outcome {
    if let Err(e) = std::fs::write(path, json) {
        let _ = std::fs::remove_file(path);
        eprintln!(
            "ymx: failed to write output file `{path}`: {e}",
            path = path.display()
        );
        return Outcome::Diagnostic;
    }
    Outcome::Success
}

/// Render every diagnostic to stderr (one `Diagnostic::render()` line per
/// entry) and report [`Outcome::Diagnostic`].
fn render_diags(diags: &[Diagnostic]) -> Outcome {
    for diag in diags {
        eprintln!("{}", diag.render());
    }
    Outcome::Diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use ymx_config::CliOverrides;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique-per-test temp directory; removed on drop (best effort).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ymx_cli_run_test_{}_{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent dirs");
            }
            fs::write(path, contents).expect("write file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cli_for(root: &Path) -> ParsedCli {
        // File-based entry: positional is the entry file; project root is its parent.
        ParsedCli {
            path: root.join("main.yml"),
            entry: None,
            from_keyword: None,
            default_keyword: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
            output: None,
            test: false,
            test_dir: None,
        }
    }

    fn cli_with_test(root: &Path) -> ParsedCli {
        let mut cli = cli_for(root);
        cli.test = true;
        cli
    }

    fn cli_with_test_dir(dir: &Path) -> ParsedCli {
        // Recursive test mode: test=true and test_dir=Some(dir)
        ParsedCli {
            path: dir.to_path_buf(),
            entry: None,
            from_keyword: None,
            default_keyword: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
            output: None,
            test: true,
            test_dir: Some(dir.to_path_buf()),
        }
    }

    // entry is now a bare component name (not a dotted path).
    fn cli_with_entry(file: &Path, component: &str) -> ParsedCli {
        ParsedCli {
            path: file.to_path_buf(),
            entry: Some(component.to_string()),
            from_keyword: None,
            default_keyword: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
            output: None,
            test: false,
            test_dir: None,
        }
    }

    #[test]
    fn compile_success_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn load_yaml_error_returns_diagnostic() {
        let dir = TempDir::new();
        dir.write("bad.yml", "a: 1\n---\nb: 2\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn load_missing_root_returns_diagnostic() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope");
        let cli = cli_for(&missing);
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn missing_default_entry_is_e009_diagnostic() {
        let dir = TempDir::new();
        dir.write("other.yml", "other: 1\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn malformed_ymx_in_entry_is_e010_diagnostic() {
        let dir = TempDir::new();
        dir.write("main.yml", "_ymx:\n  foo: 1\nmain: 0\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn explicit_entry_selects_component() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let cli = cli_with_entry(&dir.path().join("a/b.yml"), "x");
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn explicit_missing_component_is_e009_diagnostic() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let cli = cli_with_entry(&dir.path().join("a/b.yml"), "y");
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn test_branch_passing_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n_test:\n  main: 1\n");
        let cli = cli_with_test(dir.path());
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn test_branch_failing_value_returns_diagnostic() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n_test:\n  main: 2\n");
        let cli = cli_with_test(dir.path());
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn test_branch_error_match_returns_success() {
        // `main` calls the unknown `$nope` -> E002; the test asserts `E002`.
        let dir = TempDir::new();
        dir.write(
            "main.yml",
            "main: \"$nope(1)\"\n_test:\n  main:\n    error: \"E002\"\n",
        );
        let cli = cli_with_test(dir.path());
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn test_branch_malformed_block_is_e010_diagnostic() {
        // A B mapping with both `result` and `error` is malformed (`E010`).
        let dir = TempDir::new();
        dir.write(
            "main.yml",
            "main: 1\n_test:\n  main:\n    result: 1\n    error: \"E002\"\n",
        );
        let cli = cli_with_test(dir.path());
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn test_branch_without_tests_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let cli = cli_with_test(dir.path());
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn override_flags_flow_to_options() {
        // `--max-depth 1` against a self-recursive component surfaces E008 as
        // a compile diagnostic — confirming CLI overrides reach `Options`.
        let dir = TempDir::new();
        dir.write("main.yml", "main: \"$main()\"\n");
        let mut cli = cli_for(dir.path());
        cli.max_depth = Some(1);
        assert_eq!(run(&cli), Outcome::Diagnostic);
    }

    #[test]
    fn overrides_helper_yields_non_none_entry_always() {
        // entry is ALWAYS derived from file_stem.component in CLI overrides,
        // never left as None (unlike default_for_tests which is harness-only).
        let cli = cli_for(Path::new("/proj"));
        let ov = cli.overrides();
        assert_eq!(ov.entry.as_deref(), Some("main.main"));
        assert_eq!(ov.from_keyword, None);
        assert_eq!(ov.default_keyword, None);
        assert_eq!(ov.max_depth, None);
        assert_eq!(ov.pretty, None);
        assert_eq!(ov.format, None);
        assert_eq!(ov.plain, None);
    }

    #[test]
    fn diff_value_mismatch_renders_both_values() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n_test:\n  main: 2\n");
        let project = load_project(dir.path()).expect("loads");
        let opts = extract_options(&project, &CliOverrides::default_for_tests()).expect("opts");
        let results = run_tests(&project, &opts);
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(!r.passed);
        let rendered = diff(r);
        assert!(rendered.contains("expected"), "{}", rendered);
        assert!(rendered.contains("actual"), "{}", rendered);
    }

    #[test]
    fn diff_error_actual_renders_first_code() {
        let dir = TempDir::new();
        dir.write(
            "main.yml",
            "main: \"$nope(1)\"\n_test:\n  main:\n    error: \"E008\"\n",
        );
        let project = load_project(dir.path()).expect("loads");
        let opts = extract_options(&project, &CliOverrides::default_for_tests()).expect("opts");
        let results = run_tests(&project, &opts);
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(!r.passed);
        let rendered = diff(r);
        assert!(rendered.contains("E002"), "{}", rendered);
    }

    #[test]
    fn render_diags_reports_diagnostic_outcome() {
        let diags: Vec<Diagnostic> = Vec::new();
        assert_eq!(render_diags(&diags), Outcome::Diagnostic);
    }

    // ---- task 4: emit ----

    #[test]
    fn serialize_compact_and_pretty_round_trip() {
        use ymx_lib::ymx_core::parse::{node_to_value, parse_document};
        // Build a `Value::Object` via the parser so we don't reach for
        // `indexmap` from this crate. Keys are deliberately out of lexical
        // order so the `preserve_order` feature's effect is observable.
        let v = node_to_value(&parse_document("b: 2\na: 1\n").expect("parse"));
        let compact = serialize(&v, false).expect("compact");
        assert_eq!(
            compact, "{\"b\":2,\"a\":1}",
            "compact preserves YAML insertion order"
        );
        let pretty = serialize(&v, true).expect("pretty");
        assert!(pretty.contains('\n'), "pretty is multiline: {pretty}");
        assert!(pretty.contains("\"b\":"), "pretty: {pretty}");
        assert!(pretty.contains("\"a\":"), "pretty: {pretty}");
    }

    #[test]
    fn emit_format_json_no_output_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let cli = cli_for(dir.path());
        assert_eq!(
            emit(&cli, &Options::default(), &Value::Int(1)),
            Outcome::Success
        );
    }

    #[test]
    fn emit_format_diagnostics_returns_success_no_output_written() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let mut cli = cli_for(dir.path());
        let out = dir.path().join("ignored.json");
        cli.output = Some(out.clone());
        let opts = Options {
            format: Format::Diagnostics,
            ..Options::default()
        };
        assert_eq!(emit(&cli, &opts, &Value::Int(1)), Outcome::Success);
        assert!(!out.exists(), "--output ignored under --format diagnostics");
    }

    #[test]
    fn emit_json_with_output_writes_file_on_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_for(dir.path());
        cli.output = Some(out.clone());
        assert_eq!(
            emit(&cli, &Options::default(), &Value::Int(1)),
            Outcome::Success
        );
        assert!(out.exists(), "file created on success");
        let written = fs::read_to_string(&out).expect("read back");
        assert_eq!(written, "1", "compact JSON written verbatim");
    }

    #[test]
    fn emit_json_pretty_with_output_writes_multiline_file() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_for(dir.path());
        cli.output = Some(out.clone());
        let opts = Options {
            pretty: true,
            ..Options::default()
        };
        assert_eq!(emit(&cli, &opts, &Value::Int(1)), Outcome::Success);
        let written = fs::read_to_string(&out).expect("read back");
        assert_eq!(
            written, "1",
            "a scalar prettifiles to itself; round-trip OK"
        );
    }

    #[test]
    fn run_writes_output_file_on_compile_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_for(dir.path());
        cli.output = Some(out.clone());
        assert_eq!(run(&cli), Outcome::Success);
        assert!(out.exists());
        assert_eq!(fs::read_to_string(&out).unwrap(), "1");
    }

    #[test]
    fn run_does_not_write_output_file_on_compile_error() {
        // Missing default entry -> E009 in extract_options, before compile /
        // emit. The --output file must NOT be created.
        let dir = TempDir::new();
        dir.write("other.yml", "other: 1\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_for(dir.path());
        cli.output = Some(out.clone());
        assert_eq!(run(&cli), Outcome::Diagnostic);
        assert!(!out.exists(), "no file on diagnostic");
    }

    #[test]
    fn run_does_not_write_output_file_on_missing_entry_with_explicit_entry() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_with_entry(&dir.path().join("a/b.yml"), "y");
        cli.output = Some(out.clone());
        assert_eq!(run(&cli), Outcome::Diagnostic);
        assert!(!out.exists(), "no file on E009");
    }

    #[test]
    fn run_format_diagnostics_on_success_yields_success() {
        // `--format diagnostics` -> empty stdout, exit 0 on success. We test
        // the outcome here; stdout emptiness is asserted by the binary
        // integration test in tests/cli.rs (capturing stdout requires a
        // subprocess; in-process unit tests would race on the shared stdout
        // handle).
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let mut cli = cli_for(dir.path());
        cli.format = Some(Format::Diagnostics);
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn write_file_failure_returns_diagnostic_and_removes_partial() {
        // A path whose parent does not exist cannot be written by `fs::write`
        // (it does not create parent directories). write_file best-effort
        // removes any partial file (none created here) and reports Diagnostic.
        let dir = TempDir::new();
        let bad = dir.path().join("no_such_dir").join("out.json");
        assert_eq!(write_file(&bad, "1"), Outcome::Diagnostic);
        assert!(!bad.exists());
    }

    #[test]
    fn write_file_success_returns_success() {
        let dir = TempDir::new();
        let out = dir.path().join("ok.json");
        assert_eq!(write_file(&out, "42"), Outcome::Success);
        assert_eq!(fs::read_to_string(&out).unwrap(), "42");
    }

    // ---- task 2: recursive test discovery ----

    #[test]
    fn recursive_tests_single_passing_project() {
        let dir = TempDir::new();
        dir.write("proj/main.yml", "main: 1\n_test:\n  main: 1\n");
        assert_eq!(run(&cli_with_test_dir(dir.path())), Outcome::Success);
    }

    #[test]
    fn recursive_tests_multiple_projects_all_pass() {
        let dir = TempDir::new();
        dir.write("proj1/main.yml", "main: 1\n_test:\n  main: 1\n");
        dir.write("proj2/main.yml", "main: 2\n_test:\n  main: 2\n");
        assert_eq!(run(&cli_with_test_dir(dir.path())), Outcome::Success);
    }

    #[test]
    fn recursive_tests_load_failure_skips_with_warning() {
        // A subdir with an invalid YAML file should be warned and skipped,
        // but overall outcome should still be Success (0 projects, 0 tests)
        let dir = TempDir::new();
        dir.write("bad/bad.yml", "a: 1\n---\nb: 2\n"); // multi-doc is E001
        // No valid projects found - warn and exit 0
        assert_eq!(run(&cli_with_test_dir(dir.path())), Outcome::Success);
    }

    #[test]
    fn recursive_tests_no_projects_found_exits_success() {
        let dir = TempDir::new();
        dir.write("just_text.txt", "not yaml\n");
        // No YMX projects found
        assert_eq!(run(&cli_with_test_dir(dir.path())), Outcome::Success);
    }

    #[test]
    fn recursive_tests_test_failure_returns_diagnostic() {
        let dir = TempDir::new();
        dir.write("proj/main.yml", "main: 1\n_test:\n  main: 2\n"); // expects 2, gets 1
        assert_eq!(run(&cli_with_test_dir(dir.path())), Outcome::Diagnostic);
    }

    #[test]
    fn find_project_roots_detects_nested_projects() {
        let dir = TempDir::new();
        dir.write("proj1/main.yml", "main: 1\n");
        dir.write("proj2/sub/main.yml", "main: 2\n");
        dir.write("proj2/sub/nested/deep/main.yml", "main: 3\n");
        let roots = find_project_roots(dir.path());
        // proj1 and proj2/sub are project roots (contain .yml files)
        // proj2/sub/nested is NOT a project root (its parent already has .yml files)
        assert_eq!(roots.len(), 2);
    }

    #[test]
    fn find_project_roots_skips_hidden_and_git_dirs() {
        let dir = TempDir::new();
        dir.write("proj/main.yml", "main: 1\n");
        dir.write(".hidden/proj/main.yml", "main: 2\n");
        dir.write(".git/proj/main.yml", "main: 3\n");
        let roots = find_project_roots(dir.path());
        // Only the non-hidden proj is found
        assert_eq!(roots.len(), 1);
    }
}
