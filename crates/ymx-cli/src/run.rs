//! Canonical pipeline orchestration (milestone 1.10, tasks 3 + 4 + 1.23).
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
//!   stderr and yields [`RunOutcome::Diagnostic`], best-effort removing any
//!   partially written file.
//! * `Diagnostics`: on success stdout is empty and the outcome is
//!   [`RunOutcome::Success`] (the compile-error branch is already handled as
//!   [`RunOutcome::Diagnostic`] before emit runs).
//!
//! Orchestration order (PRD §CLI):
//! 1. `load_project(path)` — any load-time diagnostic (`E001` / `E004` /
//!    `E007` / `E015`) renders all diagnostics to stderr and yields
//!    [`RunOutcome::LoadError`].
//! 2. `extract_options(&project, &cli.overrides())` — resolves the entry path
//!    (`E009` / `E010`) the same way. Yields [`RunOutcome::OptionsError`].
//! 3. `--test`: `parse_tests(&project)` first (its `Err` surfaces a malformed
//!    `_test` block, `E010`, before any test runs — `run_tests` internally
//!    re-parses and silently degrades, so `parse_tests` is the gate), then
//!    `run_tests(&project, &opts)`. One line per test (`PASS` / `FAIL` + diff
//!    on failure); any failure or parse error yields
//!    [`RunOutcome::Diagnostic`]. No JSON is emitted under `--test`. In
//!    recursive mode, a `_test._build_error` key in `main.yml` asserts that
//!    `load_project` or `extract_options` fails with the given code — a match
//!    is a PASS, a mismatch is a FAIL, and no key means a failure is a warning.
//! 4. Otherwise `compile(&project, &opts)` — any diagnostic renders to
//!    stderr and yields [`RunOutcome::Diagnostic`]; success hits [`emit`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};

use indexmap::IndexMap;

use yaml_rust2::{Yaml, YamlLoader};

use ymx_config::{extract_options, CliOverrides};
use ymx_lib::ymx_core::ir::Args;
use ymx_lib::ymx_core::project::{Format, Options, Project};
use ymx_lib::ymx_core::resolve::{compile, compile_component};
use ymx_lib::{load_project, Diagnostic, Value};
use ymx_test::{parse_tests, run_tests, Expected, TestResult};

use crate::args::ParsedCli;
use crate::diagnostic::render_with_guidance;

/// The orchestration outcome, mapped to a process [`ExitCode`] by [`main`].
/// Kept as a small enum rather than returning [`ExitCode`] directly so the
/// pipeline is unit-testable (`ExitCode` does not implement `PartialEq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Success — the pipeline completed without any diagnostic or test
    /// failure.
    Success,
    /// A load-time diagnostic was rendered to stderr (E001/E004/E007/E015).
    /// Exit code 2 per milestone 1.23.
    LoadError,
    /// A compile-time diagnostic or test failure was rendered to stderr.
    /// Exit code 1 per PRD §Exit codes.
    Diagnostic,
    /// A CLI usage error (e.g. empty stdin in script mode).
    /// Exit code 2 per PRD §Exit codes.
    UsageError,
}

impl RunOutcome {
    /// Map to the process exit code:
    /// - `0` for [`RunOutcome::Success`]
    /// - `2` for [`RunOutcome::LoadError`] / [`RunOutcome::UsageError`]
    /// - `1` for [`RunOutcome::Diagnostic`]
    pub fn to_exit_code(self) -> ExitCode {
        match self {
            RunOutcome::Success => ExitCode::SUCCESS,
            RunOutcome::LoadError | RunOutcome::UsageError => ExitCode::from(2),
            RunOutcome::Diagnostic => ExitCode::from(1),
        }
    }
}

/// Panic-safe temporary project directory for stdin-as-script mode.
/// The directory is removed when this struct drops (best-effort on Windows).
struct TempProjDir(PathBuf);

impl TempProjDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let nonce = format!(
            "ymx-stdin-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let path = std::env::temp_dir().join(nonce);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempProjDir(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempProjDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Convert a parsed stdin Value to `Args` for `compile_component`.
fn value_to_args(value: &Value) -> Args {
    match value {
        Value::Object(m) => {
            let mut pairs: Vec<(String, Value)> =
                m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Args::Named(pairs)
        }
        Value::Array(a) => Args::Positional(a.clone()),
        other => Args::Positional(vec![other.clone()]),
    }
}

/// Convert a `serde_json::Value` to a `ymx_lib::Value`.
fn serde_json_value_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            Value::Array(arr.iter().map(serde_json_value_to_value).collect())
        }
        serde_json::Value::Object(m) => {
            let map: IndexMap<String, Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), serde_json_value_to_value(v)))
                .collect();
            Value::Object(map)
        }
    }
}

/// Convert a yaml-rust2 `Yaml` node to a `Value`. Returns `None` for types
/// that cannot be represented (e.g. `Yaml::Alias`).
fn yaml_to_value(yaml: &Yaml) -> Option<Value> {
    use yaml_rust2::Yaml::{Array, Boolean, Hash, Integer, Null, Real, String as YamlString};
    match yaml {
        Null => Some(Value::Null),
        Boolean(b) => Some(Value::Bool(*b)),
        Integer(i) => Some(Value::Int(*i)),
        Real(r) => {
            if let Ok(i) = r.parse::<i64>() {
                Some(Value::Int(i))
            } else if let Ok(f) = r.parse::<f64>() {
                Some(Value::Float(f))
            } else {
                Some(Value::String(r.clone()))
            }
        }
        YamlString(s) => Some(Value::String(s.clone())),
        Array(arr) => {
            let vals: Option<Vec<Value>> = arr.iter().map(yaml_to_value).collect();
            vals.map(Value::Array)
        }
        Hash(h) => {
            let mut map = IndexMap::new();
            for (k, v) in h.iter() {
                let key = match k {
                    YamlString(s) => s.clone(),
                    Integer(i) => i.to_string(),
                    _ => continue,
                };
                if let Some(val) = yaml_to_value(v) {
                    map.insert(key, val);
                }
            }
            Some(Value::Object(map))
        }
        _ => None,
    }
}

/// Drive the canonical pipeline against `cli`.
pub fn run(cli: ParsedCli) -> RunOutcome {
    // Recursive directory mode (--test with a directory path) — unaffected by stdin.
    if let Some(ref test_dir) = cli.test_dir {
        return run_recursive_tests(test_dir);
    }

    // Handle stdin-as-script mode: read stdin as the YAML document.
    // _temp_dir is kept alive for the entire function so Drop runs on return/panic.
    let _temp_dir: Option<TempProjDir>;
    let effective_path: PathBuf;

    if cli.stdin_is_script {
        let raw = match std::io::read_to_string(std::io::stdin()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ymx: failed to read stdin: {e}");
                return RunOutcome::Diagnostic;
            }
        };
        if raw.is_empty() {
            eprintln!("ymx: missing script — no path given and no content provided via stdin — usage: `ymx [path] [flags]`");
            return RunOutcome::UsageError;
        }
        let temp = TempProjDir::new();
        let script_path = temp.path().join("main.yml");
        if let Err(e) = std::fs::write(&script_path, &raw) {
            eprintln!("ymx: failed to write temp script: {e}");
            return RunOutcome::Diagnostic;
        }
        // Keep temp_dir alive for the duration of the function.
        _temp_dir = Some(temp);
        effective_path = script_path;
    } else {
        _temp_dir = None;
        effective_path = cli.path.clone();
    }

    // Use effective_path for project_root (the entry file's directory).
    let project_root = effective_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let project = match load_project(&project_root) {
        Ok(project) => project,
        Err(diags) => return render_diags_load_error(&diags),
    };

    let overrides = cli.overrides();
    let opts = match extract_options(&project, &overrides) {
        Ok(opts) => opts,
        Err(diags) => return render_diags(&diags),
    };

    if cli.test {
        // --test is unaffected by stdin modes; temp_dir is unused here.
        return run_test_branch(&project, &opts);
    }

    // Args-from-stdin mode: positional was given and stdin is non-tty.
    // Read stdin as call arguments (JSON first, YAML fallback), then call
    // compile_component instead of compile.
    if !cli.stdin_is_script {
        let stdin_content = match std::io::read_to_string(std::io::stdin()) {
            Ok(s) if !s.is_empty() => Some(s),
            Ok(_) => None, // empty stdin → no args
            Err(e) => {
                eprintln!("ymx: failed to read stdin args: {e}");
                return RunOutcome::Diagnostic;
            }
        };

        if let Some(raw) = stdin_content {
            // Try JSON first, then YAML fallback.
            let value: Value = match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(json_v) => serde_json_value_to_value(&json_v),
                Err(_) => {
                    // Retry as YAML.
                    let docs = match YamlLoader::load_from_str(&raw) {
                        Ok(d) => d,
                        Err(e) => {
                            eprintln!("ymx: stdin args: could not parse as JSON or YAML: {e}");
                            return RunOutcome::Diagnostic;
                        }
                    };
                    let doc = match docs.first() {
                        Some(d) => d,
                        None => {
                            eprintln!("ymx: stdin args: empty YAML document");
                            return RunOutcome::Diagnostic;
                        }
                    };
                    match yaml_to_value(doc) {
                        Some(v) => v,
                        None => {
                            eprintln!("ymx: stdin args: could not convert YAML to value");
                            return RunOutcome::Diagnostic;
                        }
                    }
                }
            };

            let args = value_to_args(&value);
            // Extract the bare component name from the entry override.
            let entry_component = overrides
                .entry
                .as_ref()
                .and_then(|e| e.split('.').next_back())
                .unwrap_or("main");
            match compile_component(&project, entry_component, &args, &opts) {
                Ok(v) => return emit(&cli, &opts, &v),
                Err(diags) => return render_diags(&diags),
            }
        }
    }

    // Normal compile (no positional, no stdin args, or stdin was empty).
    match compile(&project, &opts) {
        Ok(value) => emit(&cli, &opts, &value),
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

/// Returns the expected build-error code from `proj_dir/main.yml`'s top-level
/// `_test._build_error` key, or `None` if not present.
fn build_error_code(proj_dir: &Path) -> Option<String> {
    let main_yml = proj_dir.join("main.yml");
    let main_yaml = proj_dir.join("main.yaml");
    let contents = if main_yml.exists() {
        std::fs::read_to_string(&main_yml)
    } else if main_yaml.exists() {
        std::fs::read_to_string(&main_yaml)
    } else {
        return None;
    };
    let Ok(contents) = contents else {
        return None;
    };
    let Ok(docs) = YamlLoader::load_from_str(&contents) else {
        return None;
    };
    let Some(Yaml::Hash(top)) = docs.first().cloned() else {
        return None;
    };
    let Some(Yaml::Hash(test_block)) = top.get(&Yaml::String("_test".into())) else {
        return None;
    };
    let Some(Yaml::String(code)) = test_block.get(&Yaml::String("_build_error".into())) else {
        return None;
    };
    Some(code.clone())
}

/// Recursive directory test mode: discover all project roots under `dir` and
/// run tests in each. Load/extract failures with a matching `_test._build_error`
/// code are PASS; mismatches are FAIL; failures without a key are warned and
/// skipped. Parse failures and test failures cause non-zero exit.
fn run_recursive_tests(dir: &Path) -> RunOutcome {
    let roots = find_project_roots(dir);

    if roots.is_empty() {
        eprintln!("ymx: no YMX projects found in {}", dir.display());
        return RunOutcome::Success;
    }

    let mut total_passed = 0usize;
    let mut total_tests = 0usize;
    let mut overall_success = true;

    for proj_dir in &roots {
        let relpath = proj_dir
            .strip_prefix(dir)
            .unwrap_or(proj_dir.as_path())
            .to_string_lossy();

        let build_error = build_error_code(proj_dir);

        let project = match load_project(proj_dir) {
            Ok(p) => p,
            Err(diags) => {
                if let Some(ref expected_code) = build_error {
                    if diags.iter().any(|d| d.code == *expected_code) {
                        println!("PASS {}: _build_error", relpath);
                        total_passed += 1;
                    } else {
                        let actual_code = diags
                            .first()
                            .map(|d| d.code.to_string())
                            .unwrap_or_default();
                        println!(
                            "FAIL {}: _build_error mismatch (expected {}, got {})",
                            relpath, expected_code, actual_code
                        );
                        overall_success = false;
                    }
                } else {
                    eprintln!("ymx: warning: {}: {}", proj_dir.display(), diags[0].message);
                }
                continue;
            }
        };

        let opts = match extract_options(&project, &CliOverrides::default_for_tests()) {
            Ok(o) => o,
            Err(diags) => {
                if let Some(ref expected_code) = build_error {
                    if diags.iter().any(|d| d.code == *expected_code) {
                        println!("PASS {}: _build_error", relpath);
                        total_passed += 1;
                    } else {
                        let actual_code = diags
                            .first()
                            .map(|d| d.code.to_string())
                            .unwrap_or_default();
                        println!(
                            "FAIL {}: _build_error mismatch (expected {}, got {})",
                            relpath, expected_code, actual_code
                        );
                        overall_success = false;
                    }
                } else {
                    eprintln!("ymx: warning: {}: {}", proj_dir.display(), diags[0].message);
                }
                continue;
            }
        };

        if let Err(diags) = parse_tests(&project) {
            eprintln!(
                "ymx: warning: {}: parse error: {}",
                proj_dir.display(),
                diags[0].message
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

    println!(
        "PASS: {}/{} across {} project(s)",
        total_passed,
        total_tests,
        roots.len()
    );

    if overall_success && total_passed == total_tests {
        RunOutcome::Success
    } else {
        RunOutcome::Diagnostic
    }
}

/// `--test` branch: parse tests first (its `Err` is the malformed-`_test`
/// gate, surfaced as `E010`), then run them and print one line per result.
fn run_test_branch(project: &Project, opts: &Options) -> RunOutcome {
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
        RunOutcome::Success
    } else {
        RunOutcome::Diagnostic
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
///   [`RunOutcome::Success`] (PRD: `--format diagnostics` on a successful
///   compile leaves stdout empty and exits `0`). `--output` is ignored in
///   this mode.
/// * [`Format::Json`]: serialize `value` with `serde_json` (pretty iff
///   `opts.pretty`). If `cli.output` is set, the JSON is written to that
///   file atomically — the string is materialized first, so a serialization
///   failure aborts before any file is created; a write failure prints a
///   diagnostic-style error to stderr, best-effort removes any partially
///   written file, and yields [`RunOutcome::Diagnostic`]. Otherwise the JSON is
///   written to stdout. `--output` is ignored under `--test` (handled
///   earlier in [`run`]).
fn emit(cli: &ParsedCli, opts: &Options, value: &Value) -> RunOutcome {
    match opts.format {
        Format::Diagnostics => RunOutcome::Success,
        // JSON format uses pretty by default (milestone 1.23: "JSON output should
        // use `--pretty` by default"); the CLI --pretty flag overrides for compact
        Format::Json => emit_json(cli, true, value),
        // Compact format uses opts.pretty (false by default, true if --pretty given)
        Format::Compact => emit_json(cli, opts.pretty, value),
    }
}

/// Serialize `value` to JSON (pretty iff `pretty`) and dispatch to
/// `--output` or stdout. Serialization is materialized into a `String`
/// before any I/O, so a serialize failure never creates a file.
fn emit_json(cli: &ParsedCli, pretty: bool, value: &Value) -> RunOutcome {
    let json = match serialize(value, pretty) {
        Ok(json) => json,
        Err(message) => {
            eprintln!("ymx: {message}");
            return RunOutcome::Diagnostic;
        }
    };
    match cli.output.as_deref() {
        Some(path) => write_file(path, &json),
        None => {
            print!("{json}");
            RunOutcome::Success
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
/// and yields [`RunOutcome::Diagnostic`].
fn write_file(path: &Path, json: &str) -> RunOutcome {
    if let Err(e) = std::fs::write(path, json) {
        let _ = std::fs::remove_file(path);
        eprintln!(
            "ymx: failed to write output file `{path}`: {e}",
            path = path.display()
        );
        return RunOutcome::Diagnostic;
    }
    RunOutcome::Success
}

/// Render every diagnostic to stderr (one enhanced `render_with_guidance()` line
/// per entry) and report [`RunOutcome::Diagnostic`].
fn render_diags(diags: &[Diagnostic]) -> RunOutcome {
    for diag in diags {
        eprintln!("{}", render_with_guidance(diag));
    }
    RunOutcome::Diagnostic
}

/// Render every load-error diagnostic to stderr and report
/// [`RunOutcome::LoadError`].
fn render_diags_load_error(diags: &[Diagnostic]) -> RunOutcome {
    for diag in diags {
        eprintln!("{}", render_with_guidance(diag));
    }
    RunOutcome::LoadError
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
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
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
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: true,
            test_dir: Some(dir.to_path_buf()),
            stdin_is_script: false,
        }
    }

    // entry is now a bare component name (not a dotted path).
    fn cli_with_entry(file: &Path, component: &str) -> ParsedCli {
        ParsedCli {
            path: file.to_path_buf(),
            entry: Some(component.to_string()),
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
        }
    }

    #[test]
    fn compile_success_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn load_yaml_error_returns_load_error() {
        let dir = TempDir::new();
        dir.write("bad.yml", "a: 1\n---\nb: 2\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(cli), RunOutcome::LoadError);
    }

    #[test]
    fn load_missing_root_returns_load_error() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope");
        let cli = cli_for(&missing);
        assert_eq!(run(cli), RunOutcome::LoadError);
    }

    #[test]
    fn missing_default_entry_is_e009_diagnostic() {
        let dir = TempDir::new();
        dir.write("other.yml", "other: 1\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(cli), RunOutcome::Diagnostic);
    }

    #[test]
    fn malformed_ymx_in_entry_is_e010_diagnostic() {
        let dir = TempDir::new();
        dir.write("main.yml", "_ymx:\n  foo: 1\nmain: 0\n");
        let cli = cli_for(dir.path());
        assert_eq!(run(cli), RunOutcome::Diagnostic);
    }

    #[test]
    fn explicit_entry_selects_component() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let cli = cli_with_entry(&dir.path().join("a/b.yml"), "x");
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn explicit_missing_component_is_e009_diagnostic() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let cli = cli_with_entry(&dir.path().join("a/b.yml"), "y");
        assert_eq!(run(cli), RunOutcome::Diagnostic);
    }

    #[test]
    fn test_branch_passing_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n_test:\n  main: 1\n");
        let cli = cli_with_test(dir.path());
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn test_branch_failing_value_returns_diagnostic() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n_test:\n  main: 2\n");
        let cli = cli_with_test(dir.path());
        assert_eq!(run(cli), RunOutcome::Diagnostic);
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
        assert_eq!(run(cli), RunOutcome::Success);
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
        assert_eq!(run(cli), RunOutcome::Diagnostic);
    }

    #[test]
    fn test_branch_without_tests_returns_success() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        let cli = cli_with_test(dir.path());
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn override_flags_flow_to_options() {
        // `--max-depth 1` against a self-recursive component surfaces E008 as
        // a compile diagnostic — confirming CLI overrides reach `Options`.
        let dir = TempDir::new();
        dir.write("main.yml", "main: \"$main()\"\n");
        let mut cli = cli_for(dir.path());
        cli.max_depth = Some(1);
        assert_eq!(run(cli), RunOutcome::Diagnostic);
    }

    #[test]
    fn overrides_helper_yields_non_none_entry_always() {
        // entry is ALWAYS derived from file_stem.component in CLI overrides,
        // never left as None (unlike default_for_tests which is harness-only).
        let cli = cli_for(Path::new("/proj"));
        let ov = cli.overrides();
        assert_eq!(ov.entry.as_deref(), Some("main.main"));
        assert_eq!(ov.max_depth, None);
        assert_eq!(ov.pretty, None);
        assert_eq!(ov.format, None);
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
        assert_eq!(render_diags(&diags), RunOutcome::Diagnostic);
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
            RunOutcome::Success
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
        assert_eq!(emit(&cli, &opts, &Value::Int(1)), RunOutcome::Success);
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
            RunOutcome::Success
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
        assert_eq!(emit(&cli, &opts, &Value::Int(1)), RunOutcome::Success);
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
        assert_eq!(run(cli), RunOutcome::Success);
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
        assert_eq!(run(cli), RunOutcome::Diagnostic);
        assert!(!out.exists(), "no file on diagnostic");
    }

    #[test]
    fn run_does_not_write_output_file_on_missing_entry_with_explicit_entry() {
        let dir = TempDir::new();
        dir.write("a/b.yml", "x: 7\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_with_entry(&dir.path().join("a/b.yml"), "y");
        cli.output = Some(out.clone());
        assert_eq!(run(cli), RunOutcome::Diagnostic);
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
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn write_file_failure_returns_diagnostic_and_removes_partial() {
        // A path whose parent does not exist cannot be written by `fs::write`
        // (it does not create parent directories). write_file best-effort
        // removes any partial file (none created here) and reports Diagnostic.
        let dir = TempDir::new();
        let bad = dir.path().join("no_such_dir").join("out.json");
        assert_eq!(write_file(&bad, "1"), RunOutcome::Diagnostic);
        assert!(!bad.exists());
    }

    #[test]
    fn write_file_success_returns_success() {
        let dir = TempDir::new();
        let out = dir.path().join("ok.json");
        assert_eq!(write_file(&out, "42"), RunOutcome::Success);
        assert_eq!(fs::read_to_string(&out).unwrap(), "42");
    }

    // ---- task 2: recursive test discovery ----

    #[test]
    fn recursive_tests_single_passing_project() {
        let dir = TempDir::new();
        dir.write("proj/main.yml", "main: 1\n_test:\n  main: 1\n");
        assert_eq!(run(cli_with_test_dir(dir.path())), RunOutcome::Success);
    }

    #[test]
    fn recursive_tests_multiple_projects_all_pass() {
        let dir = TempDir::new();
        dir.write("proj1/main.yml", "main: 1\n_test:\n  main: 1\n");
        dir.write("proj2/main.yml", "main: 2\n_test:\n  main: 2\n");
        assert_eq!(run(cli_with_test_dir(dir.path())), RunOutcome::Success);
    }

    #[test]
    fn recursive_tests_load_failure_skips_with_warning() {
        // A subdir with an invalid YAML file should be warned and skipped,
        // but overall outcome should still be Success (0 projects, 0 tests)
        let dir = TempDir::new();
        dir.write("bad/bad.yml", "a: 1\n---\nb: 2\n"); // multi-doc is E001
                                                       // No valid projects found - warn and exit 0
        assert_eq!(run(cli_with_test_dir(dir.path())), RunOutcome::Success);
    }

    #[test]
    fn recursive_tests_no_projects_found_exits_success() {
        let dir = TempDir::new();
        dir.write("just_text.txt", "not yaml\n");
        // No YMX projects found
        assert_eq!(run(cli_with_test_dir(dir.path())), RunOutcome::Success);
    }

    #[test]
    fn recursive_tests_test_failure_returns_diagnostic() {
        let dir = TempDir::new();
        dir.write("proj/main.yml", "main: 1\n_test:\n  main: 2\n"); // expects 2, gets 1
        assert_eq!(run(cli_with_test_dir(dir.path())), RunOutcome::Diagnostic);
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

    // ---- stdin support helpers ----

    #[test]
    fn value_to_args_object_becomes_named_sorted() {
        use ymx_lib::ymx_core::ir::Args;
        let mut map = indexmap::IndexMap::new();
        map.insert("z".into(), Value::Int(1));
        map.insert("a".into(), Value::Int(2));
        let args = value_to_args(&Value::Object(map));
        match args {
            Args::Named(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].0, "a"); // sorted
                assert_eq!(pairs[1].0, "z");
            }
            other => panic!("expected Named, got {:?}", other),
        }
    }

    #[test]
    fn value_to_args_array_becomes_positional() {
        use ymx_lib::ymx_core::ir::Args;
        let args = value_to_args(&Value::Array(vec![Value::Int(1), Value::Int(2)]));
        match args {
            Args::Positional(vals) => {
                assert_eq!(vals.len(), 2);
                assert_eq!(vals[0], Value::Int(1));
            }
            other => panic!("expected Positional, got {:?}", other),
        }
    }

    #[test]
    fn value_to_args_scalar_becomes_positional_singleton() {
        use ymx_lib::ymx_core::ir::Args;
        let args = value_to_args(&Value::Int(42));
        match args {
            Args::Positional(vals) => {
                assert_eq!(vals.len(), 1);
                assert_eq!(vals[0], Value::Int(42));
            }
            other => panic!("expected Positional, got {:?}", other),
        }
    }

    #[test]
    fn serde_json_value_to_value_converts_all_types() {
        use serde_json::json;
        let v = json!({"a": 1, "b": [1, 2, 3], "c": true, "d": null, "e": "hi"});
        let value = serde_json_value_to_value(&v);
        match value {
            Value::Object(m) => {
                assert_eq!(m.get("a"), Some(&Value::Int(1)));
                assert_eq!(m.get("c"), Some(&Value::Bool(true)));
                assert_eq!(m.get("d"), Some(&Value::Null));
                assert_eq!(m.get("e"), Some(&Value::String("hi".into())));
                assert!(matches!(m.get("b"), Some(&Value::Array(_))));
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn yaml_to_value_converts_all_yaml_types() {
        let docs = YamlLoader::load_from_str("a: 1\nb: [1, 2]\nc: true\nd: ~\ne: 1.5\nf: hello\n")
            .unwrap();
        let doc = docs.first().expect("has doc");
        let value = yaml_to_value(doc).expect("converts");
        match value {
            Value::Object(m) => {
                assert_eq!(m.get("a"), Some(&Value::Int(1)));
                assert!(matches!(m.get("b"), Some(&Value::Array(_))));
                assert_eq!(m.get("c"), Some(&Value::Bool(true)));
                assert_eq!(m.get("d"), Some(&Value::Null));
                // Real number preserved as float
                assert_eq!(m.get("e"), Some(&Value::Float(1.5)));
                assert_eq!(m.get("f"), Some(&Value::String("hello".into())));
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn yaml_to_value_array_converts_correctly() {
        let docs = YamlLoader::load_from_str("[1, two, 3.0]\n").unwrap();
        let doc = docs.first().expect("has doc");
        let value = yaml_to_value(doc).expect("converts");
        match value {
            Value::Array(vals) => {
                assert_eq!(vals.len(), 3);
                assert_eq!(vals[0], Value::Int(1));
                assert_eq!(vals[1], Value::String("two".into()));
                assert_eq!(vals[2], Value::Float(3.0));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn yaml_to_value_integer_preserved_as_int() {
        let docs = YamlLoader::load_from_str("42\n").unwrap();
        let value = yaml_to_value(docs.first().expect("has doc")).expect("converts");
        assert_eq!(value, Value::Int(42));
    }
}
