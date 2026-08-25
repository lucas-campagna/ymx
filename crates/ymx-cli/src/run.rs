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

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use indexmap::IndexMap;

use yaml_rust2::{Yaml, YamlLoader};

use ymx_config::{extract_options, CliOverrides};
use ymx_lib::ymx_core::ir::Args;
use ymx_lib::ymx_core::project::{Format, Options, Project};
#[cfg(feature = "pdf-bundled")]
use ymx_lib::ymx_core::render::BundledChromeBackend;
use ymx_lib::ymx_core::render::{
    pretty_print_html, DefaultHtmlRenderer, HtmlRenderer, PdfBackend, PdfError, SystemChromeBackend,
};
use ymx_lib::ymx_core::resolve::{compile, compile_component};
use ymx_lib::{load_project, load_project_with_override, Diagnostic, StdExecutor, Value};
use ymx_test::{parse_tests, run_tests, Expected, TestResult};

use crate::args::ParsedCli;
use crate::diagnostic::render_with_guidance;

fn expand_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

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

    // Handle -c/--code inline script mode and stdin-as-script mode.
    // _temp_dir is kept alive for the entire function so Drop runs on return/panic.
    let _temp_dir: Option<TempProjDir>;
    let effective_path: PathBuf;
    let use_override_load: bool;

    if let Some(ref code) = cli.code {
        if cli.stdin_is_script {
            // Mode 3: -c only, stdin will provide args later.
            let temp = TempProjDir::new();
            let script_path = temp.path().join("main.yml");
            if let Err(e) = std::fs::write(&script_path, expand_escapes(code)) {
                eprintln!("ymx: failed to write temp script: {e}");
                return RunOutcome::Diagnostic;
            }
            _temp_dir = Some(temp);
            effective_path = script_path;
            use_override_load = false;
        } else if cli.path.exists() && cli.path.is_file() {
            // Mode 2 or 4: file + -c — load file then overlay -c components.
            _temp_dir = None;
            effective_path = cli.path.clone();
            use_override_load = true;
        } else {
            // Mode 1: -c only, no file (or path doesn't exist).
            let temp = TempProjDir::new();
            let script_path = temp.path().join("main.yml");
            if let Err(e) = std::fs::write(&script_path, expand_escapes(code)) {
                eprintln!("ymx: failed to write temp script: {e}");
                return RunOutcome::Diagnostic;
            }
            _temp_dir = Some(temp);
            effective_path = script_path;
            use_override_load = false;
        }
    } else if cli.stdin_is_script {
        // Original stdin-as-script mode: read stdin as the YAML document.
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
        if let Err(e) = std::fs::write(&script_path, expand_escapes(&raw)) {
            eprintln!("ymx: failed to write temp script: {e}");
            return RunOutcome::Diagnostic;
        }
        // Keep temp_dir alive for the duration of the function.
        _temp_dir = Some(temp);
        effective_path = script_path;
        use_override_load = false;
    } else {
        _temp_dir = None;
        effective_path = cli.path.clone();
        use_override_load = false;
    }

    // Use effective_path for project_root (the entry file's directory).
    let _project_root = effective_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Load project — with or without -c override.
    let project = if use_override_load {
        match load_project_with_override(&effective_path, cli.code.as_deref()) {
            Ok(project) => project,
            Err(diags) => return render_diags_load_error(&diags),
        }
    } else {
        match load_project(&effective_path) {
            Ok(project) => project,
            Err(diags) => return render_diags_load_error(&diags),
        }
    };

    // For temp-file modes (stdin-is-script or -c-only), effective_path is a
    // temp main.yml, so use "main.main". For file-based modes, use cli.overrides().
    let overrides = if _temp_dir.is_some() {
        CliOverrides {
            entry: Some("main.main".to_string()),
            max_depth: cli.max_depth,
            pretty: cli.pretty,
            format: cli.format.clone(),
            plain: None,
            allowed_backends: cli.allowed_backends.clone(),
            pdf_backend: cli.pdf_backend.map(|k| match k {
                crate::args::PdfBackendKind::System => "system".to_string(),
                crate::args::PdfBackendKind::Bundled => "bundled".to_string(),
                crate::args::PdfBackendKind::Docker => "docker".to_string(),
            }),
        }
    } else {
        cli.overrides()
    };
    let mut opts = match extract_options(&project, &overrides) {
        Ok(opts) => opts,
        Err(diags) => return render_diags(&diags),
    };

    // Inject the default command executor so shell execution works out of the box.
    opts.executor = Some(Arc::new(StdExecutor));

    // --no-exec disables shell execution entirely.
    if cli.no_exec {
        opts.executor = None;
    }

    if cli.test {
        // --test is unaffected by stdin modes; temp_dir is unused here.
        return run_test_branch(&project, &opts);
    }

    // Args-from-stdin mode: if stdin has data (non-tty), read and use it as args.
    // If stdin is a tty, just compile without args.
    // When -c is present, stdin always provides args (even when no positional was given,
    // which set stdin_is_script=true in the args parser).
    if !std::io::stdin().is_terminal() && (cli.code.is_some() || !cli.stdin_is_script) {
        let stdin_content = match std::io::read_to_string(std::io::stdin()) {
            Ok(s) if !s.is_empty() => Some(s),
            Ok(_) => None, // empty stdin → no args
            Err(e) => {
                eprintln!("ymx: failed to read stdin args: {e}");
                return RunOutcome::Diagnostic;
            }
        };

        if let Some(raw) = stdin_content {
            // Try JSON first (raw), then YAML (raw), then YAML (expanded escapes).
            // JSON already handles \n/\t natively, so no expansion needed there.
            // YAML needs expansion for shell convenience (e.g. echo 'x: 1\ny: 2' | ymx ...).
            let value: Value = match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(json_v) => serde_json_value_to_value(&json_v),
                Err(_) => {
                    // Try YAML with raw content first.
                    let parse_yaml = |s: &str| -> Option<Value> {
                        let docs = YamlLoader::load_from_str(s).ok()?;
                        let doc = docs.first()?;
                        yaml_to_value(doc)
                    };
                    match parse_yaml(&raw) {
                        Some(v) => v,
                        None => {
                            // Retry YAML with expanded escapes.
                            let expanded = expand_escapes(&raw);
                            match parse_yaml(&expanded) {
                                Some(v) => v,
                                None => {
                                    eprintln!("ymx: stdin args: could not parse as JSON or YAML");
                                    return RunOutcome::Diagnostic;
                                }
                            }
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

/// Recursively discover entry files under `dir`. A directory is a project
/// root if it contains `.yml`/`.yaml` files directly. The entry file is
/// chosen by priority: `<dirname>.yml`/`.yaml` > `main.yml`/`main.yaml` >
/// first `.yml` file. Subdirectories containing their own entry file
/// (`<subdir-name>.yml`) are recursed into as separate projects. Skips
/// `.git` and hidden directories.
fn find_project_roots(dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        // Collect .yml/.yaml files and subdirs directly in this directory
        let mut yml_files: Vec<PathBuf> = Vec::new();
        let mut subdirs: Vec<PathBuf> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if ext == "yml" || ext == "yaml" {
                            yml_files.push(path);
                        }
                    }
                } else if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name == ".git" || name.starts_with('.') {
                            continue;
                        }
                    }
                    subdirs.push(path);
                }
            }
        }

        if !yml_files.is_empty() {
            // This directory has YAML files — determine if single project or category.
            let dir_name = current.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let has_entry_match = yml_files.iter().any(|f| {
                let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                stem == dir_name || stem == "main"
            });

            if has_entry_match {
                // Single project: pick the entry file by priority
                let entry = yml_files
                    .iter()
                    .find(|f| {
                        f.file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| s == dir_name)
                            .unwrap_or(false)
                    })
                    .or_else(|| {
                        yml_files.iter().find(|f| {
                            f.file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s == "main")
                                .unwrap_or(false)
                        })
                    })
                    .cloned();
                if let Some(entry) = entry {
                    roots.push(entry);
                }
                // Recurse into subdirs with their own entry file (nested projects)
                for subdir in &subdirs {
                    if let Some(sub_name) = subdir.file_name().and_then(|n| n.to_str()) {
                        for ext in ["yml", "yaml"] {
                            if subdir.join(format!("{sub_name}.{ext}")).is_file() {
                                stack.push(subdir.clone());
                                break;
                            }
                        }
                    }
                }
            } else {
                // Category directory: each YAML file is a separate project.
                // Also recurse into subdirs with their own entry file.
                roots.extend(yml_files);
                for subdir in &subdirs {
                    if let Some(sub_name) = subdir.file_name().and_then(|n| n.to_str()) {
                        for ext in ["yml", "yaml"] {
                            if subdir.join(format!("{sub_name}.{ext}")).is_file() {
                                stack.push(subdir.clone());
                                break;
                            }
                        }
                    }
                }
            }
        } else {
            // No YAML files here — recurse into all subdirectories
            for subdir in &subdirs {
                stack.push(subdir.clone());
            }
        }
    }

    roots.sort();
    roots
}

/// Returns the expected build-error code from an entry file's top-level
/// `_test._build_error` key, or `None` if not present.
fn build_error_code(entry_file: &Path) -> Option<String> {
    let Ok(contents) = std::fs::read_to_string(entry_file) else {
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

/// Recursive directory test mode: discover all entry files under `dir` and
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

    for entry_file in &roots {
        let relpath = entry_file
            .strip_prefix(dir)
            .unwrap_or(entry_file.as_path())
            .to_string_lossy();

        let build_error = build_error_code(entry_file);

        let project = match load_project(entry_file) {
            Ok(p) => p,
            Err(diags) => {
                if let Some(ref expected_code) = build_error {
                    if diags.iter().any(|d| d.code == *expected_code) {
                        println!("PASS {}: _build_error", relpath);
                        total_tests += 1;
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
                        total_tests += 1;
                        overall_success = false;
                    }
                } else {
                    eprintln!("ymx: warning: {}: {}", relpath, diags[0].message);
                }
                continue;
            }
        };

        // Derive entry path from file stem (same logic as CLI overrides).
        let file_stem = entry_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main");
        let entry = format!("{}.{}", file_stem, "main");
        let overrides = CliOverrides {
            entry: Some(entry),
            ..CliOverrides::default_for_tests()
        };

        let opts = match extract_options(&project, &overrides) {
            Ok(o) => o,
            Err(diags) => {
                if let Some(ref expected_code) = build_error {
                    if diags.iter().any(|d| d.code == *expected_code) {
                        println!("PASS {}: _build_error", relpath);
                        total_tests += 1;
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
                        total_tests += 1;
                        overall_success = false;
                    }
                } else {
                    eprintln!("ymx: warning: {}: {}", relpath, diags[0].message);
                }
                continue;
            }
        };

        if let Err(diags) = parse_tests(&project, Some(&opts.entry)) {
            eprintln!(
                "ymx: warning: {}: parse error: {}",
                relpath, diags[0].message
            );
            overall_success = false;
            continue;
        }

        let results = run_tests(&project, &opts, Some(&opts.entry));
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
    match parse_tests(project, Some(&opts.entry)) {
        Ok(_) => {}
        Err(diags) => return render_diags(&diags),
    }
    let results = run_tests(project, opts, Some(&opts.entry));
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
        Expected::Html(html) => format!("HTML:{nl}{html}", nl = '\n'),
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
        // HTML format renders the value tree to HTML via DefaultHtmlRenderer.
        Format::Html => emit_html(cli, value),
        // PDF format renders the value tree to HTML then converts to PDF.
        Format::Pdf => emit_pdf(cli, value),
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

/// Render `value` to HTML via [`DefaultHtmlRenderer`] and dispatch to
/// `--output` or stdout.
fn emit_html(cli: &ParsedCli, value: &Value) -> RunOutcome {
    let html = DefaultHtmlRenderer.render_html(value);
    let output = if cli.pretty.unwrap_or(false) {
        pretty_print_html(&html)
    } else {
        html
    };
    match cli.output.as_deref() {
        Some(path) => write_file(path, &output),
        None => {
            print!("{output}");
            RunOutcome::Success
        }
    }
}

/// Render `value` to PDF via [`DefaultHtmlRenderer`] + [`PdfBackend`] and
/// dispatch binary output to `--output` or stdout.
fn emit_pdf(cli: &ParsedCli, value: &Value) -> RunOutcome {
    let html = DefaultHtmlRenderer.render_html(value);
    let pdf_bytes: Result<Vec<u8>, PdfError> = match cli.pdf_backend {
        None | Some(crate::args::PdfBackendKind::System) => {
            let backend = SystemChromeBackend;
            backend.render(&html)
        }
        #[cfg(feature = "pdf-bundled")]
        Some(crate::args::PdfBackendKind::Bundled) => {
            let backend = BundledChromeBackend;
            backend.render(&html)
        }
        #[cfg(not(feature = "pdf-bundled"))]
        Some(crate::args::PdfBackendKind::Bundled) => Err(PdfError {
            message: "bundled backend not available: rebuild with --features pdf-bundled"
                .to_string(),
        }),
        Some(crate::args::PdfBackendKind::Docker) => render_pdf_docker(&html),
    };
    match pdf_bytes {
        Ok(bytes) => match cli.output.as_deref() {
            Some(path) => write_file_binary(path, &bytes),
            None => {
                if let Err(e) = std::io::stdout().lock().write_all(&bytes) {
                    eprintln!("ymx: failed to write PDF to stdout: {e}");
                    return RunOutcome::Diagnostic;
                }
                RunOutcome::Success
            }
        },
        Err(e) => {
            eprintln!("ymx: PDF render error: {}", e.message);
            RunOutcome::Diagnostic
        }
    }
}

/// Render HTML to PDF using Docker (pdfix/html-to-pdf image).
pub(crate) fn render_pdf_docker(html: &str) -> Result<Vec<u8>, PdfError> {
    use std::fs;
    use std::process::Command;

    let cwd = std::env::current_dir().map_err(|e| PdfError {
        message: e.to_string(),
    })?;
    let html_path = cwd.join("index.html");
    let pdf_path = cwd.join("convert.pdf");

    fs::write(&html_path, html).map_err(|e| PdfError {
        message: e.to_string(),
    })?;

    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &format!("{}:/data/", cwd.display()),
            "-w",
            "/data/",
            "pdfix/html-to-pdf:latest",
            "html-to-pdf",
            "-i",
            "index.html",
            "-o",
            "convert.pdf",
        ])
        .output()
        .map_err(|e| PdfError {
            message: format!("docker run failed: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PdfError {
            message: format!("docker exited with {}: {}", output.status, stderr),
        });
    }

    let pdf_bytes = fs::read(&pdf_path).map_err(|e| PdfError {
        message: format!("failed to read convert.pdf: {}", e),
    })?;

    let _ = fs::remove_file(&html_path);
    let _ = fs::remove_file(&pdf_path);

    Ok(pdf_bytes)
}

/// Write binary `bytes` to `path`. A write failure prints a diagnostic-style
/// error to stderr, best-effort removes any partial file, and yields
/// [`RunOutcome::Diagnostic`].
fn write_file_binary(path: &Path, bytes: &[u8]) -> RunOutcome {
    if let Err(e) = std::fs::write(path, bytes) {
        let _ = std::fs::remove_file(path);
        eprintln!(
            "ymx: failed to write output file `{path}`: {e}",
            path = path.display()
        );
        return RunOutcome::Diagnostic;
    }
    RunOutcome::Success
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
    #[cfg(feature = "pdf-system")]
    use ymx_lib::ymx_core::render::Html2PdfRenderer;

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
            allowed_backends: None,
            no_exec: false,
            code: None,
            pdf_backend: None,
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
            allowed_backends: None,
            no_exec: false,
            code: None,
            pdf_backend: None,
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
            allowed_backends: None,
            no_exec: false,
            code: None,
            pdf_backend: None,
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
    fn missing_entry_file_is_e001_load_error() {
        // Passing a non-existent entry file returns E001 (LoadError).
        let dir = TempDir::new();
        dir.write("other.yml", "other: 1\n");
        let cli = cli_for(dir.path()); // path = dir/main.yml (does not exist)
        assert_eq!(run(cli), RunOutcome::LoadError);
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
        let results = run_tests(&project, &opts, None);
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
        let results = run_tests(&project, &opts, None);
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
    fn run_does_not_write_output_file_on_load_error() {
        // Entry file does not exist -> E001 in load_project, before compile /
        // emit. The --output file must NOT be created.
        let dir = TempDir::new();
        dir.write("other.yml", "other: 1\n");
        let out = dir.path().join("out.json");
        let mut cli = cli_for(dir.path()); // path = dir/main.yml (does not exist)
        cli.output = Some(out.clone());
        assert_eq!(run(cli), RunOutcome::LoadError);
        assert!(!out.exists(), "no file on load error");
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

    // ---- expand_escapes tests ----

    #[test]
    fn expand_escapes_newline() {
        assert_eq!(expand_escapes("hello\\nworld"), "hello\nworld");
    }

    #[test]
    fn expand_escapes_tab() {
        assert_eq!(expand_escapes("hello\\tworld"), "hello\tworld");
    }

    #[test]
    fn expand_escapes_backslash() {
        assert_eq!(expand_escapes("hello\\\\world"), "hello\\world");
    }

    #[test]
    fn expand_escapes_passthrough_unknown() {
        assert_eq!(expand_escapes("hello\\qworld"), "hello\\qworld");
    }

    #[test]
    fn expand_escapes_trailing_backslash() {
        assert_eq!(expand_escapes("hello\\"), "hello\\");
    }

    #[test]
    fn expand_escapes_empty_string() {
        assert_eq!(expand_escapes(""), "");
    }

    #[test]
    fn expand_escapes_no_escapes() {
        assert_eq!(expand_escapes("plain text"), "plain text");
    }

    #[test]
    fn expand_escapes_multiple() {
        assert_eq!(expand_escapes("a\\nb\\tc\\\\d"), "a\nb\tc\\d");
    }

    // ---- task 6: -c / --code tests ----

    fn err_of(parts: &[&str]) -> crate::args::ParseError {
        let args: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        crate::args::parse(&args).expect_err("expected usage error")
    }

    #[test]
    fn code_only_mode_compiles_inline_script() {
        let dir = TempDir::new();
        let cli = ParsedCli {
            path: dir.path().join("main.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: true,
            allowed_backends: None,
            no_exec: false,
            code: Some("main: hello world".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_with_file_overrides_components() {
        let dir = TempDir::new();
        dir.write("a.yml", "comp1: 5\ncomp2: 10\n");
        let cli = ParsedCli {
            path: dir.path().join("a.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
            allowed_backends: None,
            no_exec: false,
            code: Some("comp1: 20\nmain: ${comp1()}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_with_file_new_components_added() {
        let dir = TempDir::new();
        dir.write("a.yml", "comp1: 10\n");
        let cli = ParsedCli {
            path: dir.path().join("a.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
            allowed_backends: None,
            no_exec: false,
            code: Some("comp2: 20\nmain: ${comp1()} + ${comp2()}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_with_test_rejected_at_parse_time() {
        let err = err_of(&["-c", "main: 1", "--test"]);
        assert!(err.message.contains("-c/--code"));
    }

    #[test]
    fn code_only_json_input() {
        let dir = TempDir::new();
        let cli = ParsedCli {
            path: dir.path().join("main.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: true,
            allowed_backends: None,
            no_exec: false,
            code: Some("{\"main\": \"${1 + 2}\"}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_only_math_expression() {
        let dir = TempDir::new();
        let cli = ParsedCli {
            path: dir.path().join("main.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: true,
            allowed_backends: None,
            no_exec: false,
            code: Some("main: ${10 / 2}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_only_component_call() {
        let dir = TempDir::new();
        let cli = ParsedCli {
            path: dir.path().join("main.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: true,
            allowed_backends: None,
            no_exec: false,
            code: Some("greeting: \"hi\"\nmain: ${greeting()}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_with_file_json_input() {
        let dir = TempDir::new();
        dir.write("a.yml", "comp1: 10\n");
        let cli = ParsedCli {
            path: dir.path().join("a.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
            allowed_backends: None,
            no_exec: false,
            code: Some("{\"comp2\": 20, \"main\": \"${comp1()} + ${comp2()}\"}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_with_file_math_expression() {
        let dir = TempDir::new();
        dir.write("a.yml", "base: 5\n");
        let cli = ParsedCli {
            path: dir.path().join("a.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
            allowed_backends: None,
            no_exec: false,
            code: Some("main: ${base() * 3 + 1}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn code_with_file_component_call() {
        let dir = TempDir::new();
        dir.write("a.yml", "greet: \"hello\"\n");
        let cli = ParsedCli {
            path: dir.path().join("a.yml"),
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            output: None,
            test: false,
            test_dir: None,
            stdin_is_script: false,
            allowed_backends: None,
            no_exec: false,
            code: Some("main: ${greet()}".to_string()),
            pdf_backend: None,
        };
        assert_eq!(run(cli), RunOutcome::Success);
    }

    // ---- task 6: -f html integration tests ----

    #[test]
    fn run_format_html_renders_simple_tag() {
        let dir = TempDir::new();
        dir.write("main.yml", "main:\n  from: div\n  children: Hello\n");
        let mut cli = cli_for(dir.path());
        cli.format = Some(Format::Html);
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn run_format_html_nested_inner_first() {
        let dir = TempDir::new();
        dir.write(
            "main.yml",
            "main:\n  from: div\n  children:\n    from: span\n    children: inner\n",
        );
        let mut cli = cli_for(dir.path());
        cli.format = Some(Format::Html);
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn run_format_html_style_and_class() {
        let dir = TempDir::new();
        dir.write(
            "main.yml",
            "main:\n  from: div\n  style: {color: red}\n  class: [a, b]\n  children: text\n",
        );
        let mut cli = cli_for(dir.path());
        cli.format = Some(Format::Html);
        assert_eq!(run(cli), RunOutcome::Success);
    }

    #[test]
    fn run_format_html_with_output_file() {
        let dir = TempDir::new();
        dir.write("main.yml", "main:\n  from: p\n  children: hi\n");
        let out = dir.path().join("out.html");
        let mut cli = cli_for(dir.path());
        cli.format = Some(Format::Html);
        cli.output = Some(out.clone());
        assert_eq!(run(cli), RunOutcome::Success);
        assert!(out.exists());
        let content = fs::read_to_string(&out).unwrap();
        assert!(content.contains("<p>"));
        assert!(content.contains("hi"));
    }

    // ---- task 7: PDF integration tests ----

    #[test]
    #[ignore] // requires Chrome/Chromium installed on the system
    #[cfg(feature = "pdf-system")]
    fn test_pdf_system_backend_renders_valid_pdf() {
        let dir = TempDir::new();
        dir.write("main.yml", "main:\n  from: div\n  children: Hello PDF\n");

        let project = load_project(dir.path()).expect("load_project should succeed");
        let opts = extract_options(&project, &CliOverrides::default_for_tests())
            .expect("extract_options should succeed");
        let value = compile(&project, &opts).expect("compile should succeed");

        // Render to HTML first
        let html = DefaultHtmlRenderer.render_html(&value);
        assert!(html.contains("<div>"), "HTML should contain div tag");

        // Render to PDF using system backend
        let renderer = Html2PdfRenderer(SystemChromeBackend);
        let bytes = renderer.render(&html).expect("render_pdf should succeed");

        // Verify PDF magic bytes
        assert!(!bytes.is_empty(), "PDF bytes should not be empty");
        assert!(
            bytes.starts_with(b"%PDF"),
            "PDF should start with %PDF magic"
        );
    }

    #[test]
    #[ignore] // requires Docker running with pdfix/html-to-pdf image
    fn test_pdf_docker_backend_renders_valid_pdf() {
        use std::process::Command;

        // Skip if docker is not available
        if Command::new("docker").arg("info").output().is_err() {
            return;
        }

        let dir = TempDir::new();
        dir.write(
            "main.yml",
            "main:\n  from: div\n  children: Hello Docker PDF\n",
        );

        let project = load_project(dir.path()).expect("load_project should succeed");
        let opts = extract_options(&project, &CliOverrides::default_for_tests())
            .expect("extract_options should succeed");
        let value = compile(&project, &opts).expect("compile should succeed");
        let html = DefaultHtmlRenderer.render_html(&value);

        // render_pdf_docker is pub(crate), call it directly
        let bytes = render_pdf_docker(&html).expect("docker render should succeed");
        assert!(
            bytes.starts_with(b"%PDF"),
            "PDF should start with %PDF magic"
        );
    }
}
