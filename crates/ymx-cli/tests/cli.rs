//! End-to-end integration tests for the `ymx` binary (milestone 1.10, task 4).
//!
//! These spawn the compiled binary via [`std::process::Command`] and assert
//! on the captured stdout / stderr / exit code — the reason the emit shape
//! needs driven here rather than at the `run::run` unit level is that in-unit
//! capture of `println!`/`eprintln!` output races on the shared process
//! stdout handle. Each scenario writes a small YMX project into a fresh
//! temp directory and invokes the binary by absolute path.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Unique-per-test temp directory; removed on drop (best effort).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ymx_cli_int_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn path(&self) -> &std::path::Path {
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

/// Invoke the binary with `args` (no program name). The temp dir's path is
/// **not** prepended — callers pass exactly the argv shape they want.
fn ymx(args: &[&str]) -> (std::process::Output, PathBuf) {
    let bin = env!("CARGO_BIN_EXE_ymx");
    let output = Command::new(bin)
        .args(args)
        .output()
        .expect("spawn ymx binary");
    (output, PathBuf::from(bin))
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn compile_success_emits_compact_json_to_stdout() {
    let dir = TempDir::new();
    dir.write("main.yml", "x: 1\nmain:\n  a: 2\n  b: 3\n");

    let out = ymx(&[dir.path().to_str().unwrap()]);
    assert!(out.0.status.success(), "stderr: {}", stderr(&out.0));
    let stdout = stdout(&out.0);
    assert_eq!(
        stdout.trim_end(),
        r#"{"a":2,"b":3}"#,
        "compact JSON, insertion order"
    );
}

#[test]
fn pretty_flag_emits_multiline_json_to_stdout() {
    let dir = TempDir::new();
    dir.write("main.yml", "main:\n  a: 2\n  b: 3\n");

    let out = ymx(&["--pretty", dir.path().to_str().unwrap()]);
    assert!(out.0.status.success(), "stderr: {}", stderr(&out.0));
    let stdout = stdout(&out.0);
    assert!(stdout.contains('\n'), "pretty is multiline: {stdout}");
    assert!(stdout.contains("\"a\":"), "pretty: {stdout}");
    assert!(stdout.contains("\"b\":"), "pretty: {stdout}");
}

#[test]
fn output_file_is_written_on_success() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 7\n");
    let out_path = dir.path().join("out.json");

    let result = ymx(&[
        "--output",
        out_path.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    ]);
    assert!(result.0.status.success(), "stderr: {}", stderr(&result.0));
    assert!(out_path.exists(), "output file created on success");
    let written = fs::read_to_string(&out_path).unwrap();
    assert_eq!(written, "7");
    // stdout is empty when --output is used.
    assert_eq!(stdout(&result.0), "", "no stdout when --output is set");
}

#[test]
fn output_file_not_created_on_compile_error() {
    let dir = TempDir::new();
    dir.write("other.yml", "other: 1\n");
    let out_path = dir.path().join("out.json");

    let result = ymx(&[
        "--output",
        out_path.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    ]);
    assert!(!result.0.status.success(), "non-zero exit on E009");
    assert!(!out_path.exists(), "no output file on diagnostic");
    let stderr = stderr(&result.0);
    assert!(stderr.contains("E009"), "stderr renders E009: {stderr}");
    assert_eq!(stdout(&result.0), "", "no stdout on diagnostic");
}

#[test]
fn output_file_not_created_on_load_error() {
    let dir = TempDir::new();
    dir.write("bad.yml", "a: 1\n---\nb: 2\n");
    let out_path = dir.path().join("out.json");

    let result = ymx(&[
        "--output",
        out_path.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    ]);
    assert!(!result.0.status.success(), "non-zero exit on load error");
    assert!(!out_path.exists(), "no output file on load error");
    assert!(stderr(&result.0).contains("E001"), "stderr renders E001");
}

#[test]
fn format_diagnostics_on_success_emits_empty_stdout_exit_zero() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");

    let out = ymx(&["--format", "diagnostics", dir.path().to_str().unwrap()]);
    assert!(out.0.status.success(), "exit 0 on success");
    assert_eq!(
        stdout(&out.0),
        "",
        "empty stdout under --format diagnostics"
    );
    assert_eq!(stderr(&out.0), "", "no stderr on success");
}

#[test]
fn format_diagnostics_on_compile_error_renders_diagnostic_to_stderr() {
    let dir = TempDir::new();
    dir.write("other.yml", "other: 1\n");

    let out = ymx(&["--format", "diagnostics", dir.path().to_str().unwrap()]);
    assert!(!out.0.status.success(), "non-zero exit on E009");
    assert_eq!(stdout(&out.0), "", "no stdout on diagnostic");
    assert!(
        stderr(&out.0).contains("E009"),
        "stderr renders E009: {}",
        stderr(&out.0)
    );
}

#[test]
fn entry_flag_selects_component() {
    let dir = TempDir::new();
    dir.write("a/b.yml", "x: 7\n");
    dir.write("main.yml", "main: 0\n");

    let out = ymx(&["--entry", "a.b.x", dir.path().to_str().unwrap()]);
    assert!(out.0.status.success(), "stderr: {}", stderr(&out.0));
    assert_eq!(stdout(&out.0).trim_end(), "7");
}

#[test]
fn ambiguous_stem_yamls_is_e009() {
    // The default entry `main.main` resolves against `main.yml` and
    // `main.yaml`. To reach the ambiguous-stem E009 (not an E004
    // duplicate-name clash first), the two co-stemmed files define
    // disjoint top-level keys — no E004 from the namespace merge.
    let dir = TempDir::new();
    dir.write("main.yml", "a: 1\n");
    dir.write("main.yaml", "b: 2\n");

    let out = ymx(&[dir.path().to_str().unwrap()]);
    assert!(!out.0.status.success(), "ambiguous stem errors");
    let stderr = stderr(&out.0);
    assert!(stderr.contains("E009"), "stderr renders E009: {stderr}");
    assert!(
        stderr.contains("ambiguous entry"),
        "ambiguous-stem message: {stderr}"
    );
}

#[test]
fn test_flag_runs_inline_tests_and_exits_zero_on_pass() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n_test:\n  main: 1\n");

    let out = ymx(&["--test", dir.path().to_str().unwrap()]);
    assert!(out.0.status.success(), "stderr: {}", stderr(&out.0));
    let stdout = stdout(&out.0);
    assert!(stdout.contains("PASS main"), "stdout: {stdout}");
    assert!(!stdout.contains("FAIL"), "no failures: {stdout}");
}

#[test]
fn test_flag_exits_nonzero_on_failure() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n_test:\n  main: 2\n");

    let out = ymx(&["--test", dir.path().to_str().unwrap()]);
    assert!(!out.0.status.success(), "non-zero exit on failing test");
    let stdout = stdout(&out.0);
    assert!(stdout.contains("FAIL main"), "stdout: {stdout}");
    assert!(stdout.contains("expected"), "diff present: {stdout}");
    assert!(stdout.contains("actual"), "diff present: {stdout}");
}

#[test]
fn test_flag_does_not_emit_json() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n_test:\n  main: 1\n");

    let out = ymx(&["--test", dir.path().to_str().unwrap()]);
    assert!(out.0.status.success(), "stderr: {}", stderr(&out.0));
    let stdout = stdout(&out.0);
    // Compile success would normally print a JSON value; under --test we
    // must emit only PASS/FAIL lines, never JSON. A leading `{"` is the
    // simplest JSON-object tell; a `_test` `main: 1` value renders as `1`,
    // so this also confirms the value didn't leak to stdout.
    assert!(
        !stdout.starts_with("{\""),
        "no JSON object under --test: {stdout}"
    );
    assert!(
        !stdout.starts_with('['),
        "no JSON array under --test: {stdout}"
    );
}

#[test]
fn plain_and_plain_template_together_errors_before_load() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");

    let out = ymx(&["--plain", "--plain-template", dir.path().to_str().unwrap()]);
    assert!(
        !out.0.status.success(),
        "non-zero exit on mutual-exclusion error"
    );
    let stderr = stderr(&out.0);
    assert!(stderr.contains("mutually exclusive"), "stderr: {stderr}");
    // No diagnostic rendered — the error fired before load_project.
    assert!(!stderr.contains("E001"), "no load touched: {stderr}");
}

#[test]
fn missing_path_is_usage_error() {
    let out = ymx(&[]);
    assert!(!out.0.status.success(), "non-zero exit on missing path");
    assert!(
        stderr(&out.0).contains("missing"),
        "stderr: {}",
        stderr(&out.0)
    );
}

#[test]
fn unknown_flag_is_usage_error() {
    let out = ymx(&["--bogus", "."]);
    assert!(!out.0.status.success(), "non-zero exit on unknown flag");
    assert!(
        stderr(&out.0).contains("unknown flag"),
        "stderr: {}",
        stderr(&out.0)
    );
}

#[test]
fn help_flag_exits_zero() {
    // Task 6: `--help` / `-h` print the manual page to stdout and exit 0.
    // Assert the exit code is exactly 0 (not just `.success()`) and that the
    // manual page lists every long flag.
    const EXPECTED_FLAGS: &[&str] = &[
        "--entry",
        "--from-keyword",
        "--default-keyword",
        "--max-depth",
        "--pretty",
        "--format",
        "--output",
        "--plain",
        "--plain-template",
        "--test",
        "--help",
        "-h",
    ];

    for arg in ["--help", "-h"] {
        let out = ymx(&[arg]);
        assert_eq!(out.0.status.code(), Some(0), "exit 0 on {arg}");
        let stdout = stdout(&out.0);
        assert!(!stdout.is_empty(), "{arg} must print the manual page");
        assert!(
            stdout.contains("USAGE") && stdout.contains("FLAGS") && stdout.contains("EXIT CODES"),
            "{arg}: manual page must have USAGE / FLAGS / EXIT CODES sections"
        );
        for flag in EXPECTED_FLAGS {
            assert!(
                stdout.contains(flag),
                "{arg}: manual page missing flag `{flag}`\n--- stdout ---\n{stdout}"
            );
        }
        // The specific contract bits the milestone calls out.
        assert!(
            stdout.contains("mutually exclusive"),
            "{arg}: manual must call out --plain/--plain-template mutual exclusion"
        );
        assert!(
            stdout.to_lowercase().contains("only on success"),
            "{arg}: manual must state the --output success-only rule:\n{stdout}"
        );
        // Usage errors and the default for each flag are documented.
        for default in [
            "main.main",
            "default: from",
            "default: default",
            "default: 256",
            "default: json",
            "default: stdout",
        ] {
            assert!(
                stdout.contains(default),
                "{arg}: manual missing default `{default}`"
            );
        }
        // No diagnostics on --help — stderr is empty.
        assert_eq!(stderr(&out.0), "", "{arg}: stderr must be empty");
    }
}

// ---------------------------------------------------------------------------
// Task 5 — exit-code matrix. Locks the contract: 0 success, 1 runtime
// diagnostic / test failure, 2 usage (arg-parse) error. Each row asserts the
// raw OS exit code via `status.code()` (not just `.success()`) so the specific
// non-zero value is pinned, not merely "non-zero".
// ---------------------------------------------------------------------------

/// Assert the raw exit code of a binary run; surfaces stdout/stderr for
/// triage on failure.
fn assert_exit(args: &[&str], expected: i32) {
    let out = ymx(args);
    let got = out.0.status.code();
    if got != Some(expected) {
        panic!(
            "exit code: expected {expected}, got {got:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout(&out.0),
            stderr(&out.0)
        );
    }
}

#[test]
fn exit_code_success_compile_json_is_zero() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(&[dir.path().to_str().unwrap()], 0);
}

#[test]
fn exit_code_success_format_diagnostics_is_zero() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(
        &["--format", "diagnostics", dir.path().to_str().unwrap()],
        0,
    );
}

#[test]
fn exit_code_success_test_run_all_passed_is_zero() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n_test:\n  main: 1\n");
    assert_exit(&["--test", dir.path().to_str().unwrap()], 0);
}

#[test]
fn exit_code_success_test_no_blocks_is_zero_noop_success() {
    // No `_test` blocks exist: parse_tests returns Ok(empty), run_tests
    // returns an empty Vec. `passed == total == 0` is a no-op success (NOT
    // a diagnostic). This locks the matrix row "no-op --test success → 0".
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(&["--test", dir.path().to_str().unwrap()], 0);
}

#[test]
fn exit_code_test_failure_is_one() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n_test:\n  main: 2\n");
    assert_exit(&["--test", dir.path().to_str().unwrap()], 1);
}

#[test]
fn exit_code_test_malformed_block_is_one() {
    // A B mapping with both `result` and `error` is malformed (`E010`) at
    // `parse_tests`; the CLI renders the diagnostic to stderr and exits 1.
    let dir = TempDir::new();
    dir.write(
        "main.yml",
        "main: 1\n_test:\n  main:\n    result: 1\n    error: \"E002\"\n",
    );
    assert_exit(&["--test", dir.path().to_str().unwrap()], 1);
}

#[test]
fn exit_code_load_error_is_one() {
    let dir = TempDir::new();
    dir.write("bad.yml", "a: 1\n---\nb: 2\n");
    assert_exit(&[dir.path().to_str().unwrap()], 1);
}

#[test]
fn exit_code_extract_options_e009_is_one() {
    let dir = TempDir::new();
    dir.write("other.yml", "other: 1\n");
    assert_exit(&[dir.path().to_str().unwrap()], 1);
}

#[test]
fn exit_code_extract_options_e010_is_one() {
    let dir = TempDir::new();
    dir.write("main.yml", "_ymx:\n  foo: 1\nmain: 0\n");
    assert_exit(&[dir.path().to_str().unwrap()], 1);
}

#[test]
fn exit_code_compile_error_is_one() {
    // `--max-depth 1` against a self-recursive component surfaces E008 at
    // `compile`. Confirms compile-step errors map to exit 1, not 2.
    let dir = TempDir::new();
    dir.write("main.yml", "main: \"$main()\"\n");
    assert_exit(&["--max-depth", "1", dir.path().to_str().unwrap()], 1);
}

#[test]
fn exit_code_missing_path_usage_is_two() {
    // Usage errors (arg-parse stage) use exit 2 to distinguish them from a
    // runtime diagnostic's exit 1 (see `main.rs`).
    assert_exit(&[], 2);
}

#[test]
fn exit_code_extra_positional_usage_is_two() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(
        &[dir.path().to_str().unwrap(), dir.path().to_str().unwrap()],
        2,
    );
}

#[test]
fn exit_code_bad_max_depth_usage_is_two() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(&["--max-depth", "abc", dir.path().to_str().unwrap()], 2);
}

#[test]
fn exit_code_bad_format_usage_is_two() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(&["--format", "xml", dir.path().to_str().unwrap()], 2);
}

#[test]
fn exit_code_unknown_flag_usage_is_two() {
    assert_exit(&["--bogus", "."], 2);
}

#[test]
fn exit_code_plain_and_plain_template_usage_is_two_no_load() {
    // Mutual exclusion fires in arg-parse, before any `load_project`. Lock
    // the contract: exit 2 (not 1), and stderr carries the usage message,
    // never an `E00x` diagnostic.
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    let out = ymx(&["--plain", "--plain-template", dir.path().to_str().unwrap()]);
    assert_eq!(out.0.status.code(), Some(2));
    let stderr = stderr(&out.0);
    assert!(stderr.contains("mutually exclusive"), "stderr: {stderr}");
    assert!(
        !stderr.contains('E'),
        "no E00x diagnostic under usage error: {stderr}"
    );
}

#[test]
fn exit_code_missing_value_usage_is_two() {
    let dir = TempDir::new();
    dir.write("main.yml", "main: 1\n");
    assert_exit(&["--entry"], 2);
    assert_exit(&["proj/", "--output"], 2);
}

#[test]
fn exit_code_help_is_zero() {
    assert_exit(&["--help"], 0);
    assert_exit(&["-h"], 0);
}
