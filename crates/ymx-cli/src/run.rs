//! Canonical pipeline orchestration (milestone 1.10, task 3).
//!
//! `load_project` -> `extract_options` -> (`--test` ? `run_tests` :
//! `compile`) -> emit. Diagnostic rendering (one
//! [`Diagnostic::render`](ymx_lib::Diagnostic::render) line per stderr entry)
//! and exit codes are this module's responsibility; the success-emit shape
//! (JSON pretty / `--output` file / `--format diagnostics` empty-stdout) lands
//! in task 4. Until then the compile-success path uses a stub emit
//! (`println!("{value:?}")`).
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
//!    [`Outcome::Diagnostic`].
//! 4. Otherwise `compile(&project, &opts)` — any diagnostic renders to
//!    stderr and yields [`Outcome::Diagnostic`]; success hits the stub emit
//!    (task 4 replaces it).

use std::process::ExitCode;

use ymx_config::extract_options;
use ymx_lib::ymx_core::project::{Options, Project};
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
    let project = match load_project(&cli.path) {
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
        Ok(value) => emit_stub(&value),
        Err(diags) => render_diags(&diags),
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

/// Stub success emit (task 4 replaces it with the real emit shape — JSON
/// pretty iff `--pretty`, `--output` file written only on success, and
/// `--format diagnostics` empty stdout).
fn emit_stub(value: &Value) -> Outcome {
    println!("{value:?}");
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
        // The orchestration overrides shape mirrors a bare invocation: no
        // flags, default entry `main.main`.
        ParsedCli {
            path: root.to_path_buf(),
            entry: None,
            from_keyword: None,
            default_keyword: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
            output: None,
            test: false,
        }
    }

    fn cli_with_test(root: &Path) -> ParsedCli {
        let mut cli = cli_for(root);
        cli.test = true;
        cli
    }

    fn cli_with_entry(root: &Path, entry: &str) -> ParsedCli {
        let mut cli = cli_for(root);
        cli.entry = Some(entry.to_string());
        cli
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
        let cli = cli_with_entry(dir.path(), "a.b.x");
        assert_eq!(run(&cli), Outcome::Success);
    }

    #[test]
    fn explicit_missing_component_is_e009_diagnostic() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let cli = cli_with_entry(dir.path(), "a.b.y");
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
    fn overrides_helper_yields_default_for_tests_when_absent() {
        let cli = cli_for(Path::new("/proj"));
        assert_eq!(cli.overrides(), CliOverrides::default_for_tests());
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
}
