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
#[cfg(feature = "watch")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use indexmap::IndexMap;

use yaml_rust2::{Yaml, YamlLoader};

use ymx_config::{extract_lint_config, extract_options, CliOverrides, LintOverrides};
use ymx_lib::ymx_core::ir::Args;
use ymx_lib::ymx_core::project::{Format, Options, Project};
use ymx_lib::ymx_core::render::{pretty_print_html, DefaultHtmlRenderer, HtmlRenderer};
use ymx_lib::ymx_core::resolve::{compile, compile_component};
use ymx_lib::{
    load_project, load_project_with_override, Diagnostic, StdExecutor, StdIpcHost, Value,
};
use ymx_lint::{
    lint_project, render_diagnostics as render_lint_diagnostics, render_json as render_lint_json,
    should_fail,
};
use ymx_test::{parse_tests, run_tests, Expected, TestResult};

use crate::args::ParsedCli;
use crate::diagnostic::render_with_guidance;

// PDF rendering types — live here in ymx-cli so ymx-core stays I/O-free.

/// Error returned when PDF rendering fails.
#[derive(Debug, Clone)]
pub struct PdfError {
    pub message: String,
}

/// Trait for rendering HTML to PDF.
#[allow(dead_code)]
pub trait PdfBackend: Send + Sync {
    fn render(&self, html: &str) -> Result<Vec<u8>, PdfError>;
}

/// PDF backend that uses ironpress (native Rust HTML-to-PDF).
pub struct IronpressBackend;

impl PdfBackend for IronpressBackend {
    fn render(&self, html: &str) -> Result<Vec<u8>, PdfError> {
        use ironpress::html_to_pdf;

        let pdf = html_to_pdf(html).map_err(|e| PdfError {
            message: e.to_string(),
        })?;
        Ok(pdf)
    }
}

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
    /// A lint finding met `fail_on`, or lint load/option resolution failed.
    /// Exit code 1 per PRD §Linting.
    LintFailure,
    /// A CLI usage error (e.g. empty stdin in script mode).
    /// Exit code 2 per PRD §Exit codes.
    UsageError,
}

impl RunOutcome {
    /// Map to the process exit code:
    /// - `0` for [`RunOutcome::Success`]
    /// - `2` for [`RunOutcome::LoadError`] / [`RunOutcome::UsageError`]
    /// - `1` for [`RunOutcome::Diagnostic`] / [`RunOutcome::LintFailure`]
    pub fn to_exit_code(self) -> ExitCode {
        match self {
            RunOutcome::Success => ExitCode::SUCCESS,
            RunOutcome::LoadError | RunOutcome::UsageError => ExitCode::from(2),
            RunOutcome::Diagnostic | RunOutcome::LintFailure => ExitCode::from(1),
        }
    }
}

#[cfg(feature = "watch")]
fn run_single_compile(cli: &ParsedCli) -> (RunOutcome, Vec<Diagnostic>, Option<String>) {
    let project = match load_project(&cli.path) {
        Ok(p) => p,
        Err(diags) => {
            return (RunOutcome::LoadError, diags, None);
        }
    };

    let overrides = cli.overrides();
    let mut opts = match extract_options(&project, &overrides) {
        Ok(o) => o,
        Err(diags) => {
            return (RunOutcome::Diagnostic, diags, None);
        }
    };

    opts.executor = Some(Arc::new(StdExecutor));
    if cli.no_exec {
        opts.executor = None;
    }

    // --no-ipc disables IPC entirely.
    if cli.no_ipc {
        opts.ipc = None;
    } else if opts.ipc.is_none() {
        if let Some(ref exec) = opts.executor {
            opts.ipc = Some(Arc::new(StdIpcHost::new(exec.clone())));
        }
    }

    match compile(&project, &opts) {
        Ok(value) => {
            let (outcome, output) = emit(cli, &opts, &value);
            (outcome, Vec::new(), output)
        }
        Err(diags) => (RunOutcome::Diagnostic, diags, None),
    }
}

#[cfg(feature = "watch")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderState {
    Success,
    Error,
}

#[cfg(feature = "watch")]
fn watch_msg_path(watch_path: &Path, cli: &ParsedCli) -> String {
    let input = watch_path.display().to_string();
    let output = cli
        .output
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "stdout".to_string());
    format!("{} -> {}", input, output)
}

/// Watch mode: runs the compile pipeline on every .yml/.yaml file change.
/// Exits cleanly on SIGINT/SIGTERM (exit 0).
#[cfg(feature = "watch")]
pub fn run_watch(cli: &ParsedCli) -> RunOutcome {
    let watch_path = cli.watch.as_ref().expect("--watch value must be present");
    println!("Watching {}...", watch_path.display());

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    if let Err(e) = ctrlc::set_handler(move || {
        shutdown_clone.store(true, Ordering::SeqCst);
    }) {
        eprintln!("ymx: warning: could not set Ctrl-C handler: {e}");
    }

    let debounce_duration = Duration::from_millis(100);
    let mut pending = false;

    // Collect current mtimes for watched path (file or directory)
    fn get_current_mtimes(path: &Path) -> Vec<(PathBuf, Option<u64>)> {
        if path.is_file() {
            vec![get_file_mtime(path)]
        } else if path.is_dir() {
            let mut mtimes = Vec::new();
            let walker = ignore::WalkBuilder::new(path)
                .max_depth(Some(10))
                .hidden(true)
                .build();
            for entry in walker.flatten() {
                let file_path = entry.path();
                if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
                    if ext == "yml" || ext == "yaml" {
                        mtimes.push(get_file_mtime(file_path));
                    }
                }
            }
            mtimes
        } else {
            Vec::new()
        }
    }

    fn get_file_mtime(path: &Path) -> (PathBuf, Option<u64>) {
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            })
            .ok();
        (path.to_path_buf(), mtime)
    }

    // Compare two mtime collections for equality
    fn mtimes_equal(a: &[(PathBuf, Option<u64>)], b: &[(PathBuf, Option<u64>)]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        // Sort both by path for comparison
        let mut a_sorted: Vec<_> = a.iter().collect();
        let mut b_sorted: Vec<_> = b.iter().collect();
        a_sorted.sort_by_key(|k| &k.0);
        b_sorted.sort_by_key(|k| &k.0);
        a_sorted
            .into_iter()
            .zip(b_sorted)
            .all(|(a, b)| a.0 == b.0 && a.1 == b.1)
    }

    // Run initial compile
    let start = Instant::now();
    println!(
        "\x1b[2J\x1b[H{}: Compiling...",
        watch_msg_path(watch_path, cli)
    );
    std::io::stdout().flush().ok();
    let (outcome, diags, _output) = run_single_compile(cli);
    let elapsed = start.elapsed();
    let state = match outcome {
        RunOutcome::Success => RenderState::Success,
        _ => RenderState::Error,
    };
    match state {
        RenderState::Success => {
            let label = format!(
                "{}: \u{2713} compiled in {:.3}s",
                watch_msg_path(watch_path, cli),
                elapsed.as_secs_f64()
            );
            print!("\x1b[2J\x1b[H{label}");
            std::io::stdout().flush().ok();
        }
        RenderState::Error => {
            print!("\x1b[2J\x1b[H\u{2717} ERROR");
            std::io::stdout().flush().ok();
            if diags.is_empty() {
                eprintln!(" (no diagnostics available)");
            } else {
                for diag in &diags {
                    eprintln!("{}", diag.render());
                }
            }
        }
    }
    if shutdown.load(Ordering::SeqCst) {
        return RunOutcome::Success;
    }
    // Don't exit on first compile error — keep watching

    // Initialize last mtimes after initial compile
    let mut last_mtimes = get_current_mtimes(watch_path);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return RunOutcome::Success;
        }

        std::thread::sleep(debounce_duration);

        let current_mtimes = get_current_mtimes(watch_path);

        // Check if anything changed
        if !mtimes_equal(&current_mtimes, &last_mtimes) {
            pending = true;
        }

        if pending {
            pending = false;
            last_mtimes = current_mtimes;

            let start = Instant::now();
            println!(
                "\x1b[2J\x1b[H{}: Compiling...",
                watch_msg_path(watch_path, cli)
            );
            std::io::stdout().flush().ok();
            let (outcome, diags, _output) = run_single_compile(cli);
            let elapsed = start.elapsed();
            let state = match outcome {
                RunOutcome::Success => RenderState::Success,
                _ => RenderState::Error,
            };
            match state {
                RenderState::Success => {
                    let label = format!(
                        "{}: \u{2713} compiled in {:.3}s",
                        watch_msg_path(watch_path, cli),
                        elapsed.as_secs_f64()
                    );
                    print!("\x1b[2J\x1b[H{label}");
                    std::io::stdout().flush().ok();
                }
                RenderState::Error => {
                    print!("\x1b[2J\x1b[H\u{2717} ERROR");
                    std::io::stdout().flush().ok();
                    if diags.is_empty() {
                        eprintln!(" (no diagnostics available)");
                    } else {
                        for diag in &diags {
                            eprintln!("{}", diag.render());
                        }
                    }
                }
            }
            if shutdown.load(Ordering::SeqCst) {
                return RunOutcome::Success;
            }
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
pub fn run(cli: &ParsedCli) -> RunOutcome {
    if cli.lint {
        return run_lint(cli);
    }

    // Watch mode: enter the file watcher loop and never return (until interrupted).
    #[cfg(feature = "watch")]
    if cli.watch.is_some() {
        return run_watch(cli);
    }

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
            allowed_ipc: cli.allowed_ipc.clone(),
            pdf_backend: cli.pdf_backend.as_ref().map(|k| match k {
                crate::args::PdfBackendKind::Ironpress => "ironpress".to_string(),
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

    // --no-ipc disables IPC entirely.
    if cli.no_ipc {
        opts.ipc = None;
    } else if opts.ipc.is_none() {
        // Inject StdIpcHost so IPC works out of the box.
        if let Some(ref exec) = opts.executor {
            opts.ipc = Some(Arc::new(StdIpcHost::new(exec.clone())));
        }
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
                Ok(v) => {
                    let (outcome, output) = emit(cli, &opts, &v);
                    if let Some(out) = output {
                        print!("{out}");
                    }
                    return outcome;
                }
                Err(diags) => return render_diags(&diags),
            }
        }
    }

    // Normal compile (no positional, no stdin args, or stdin was empty).
    match compile(&project, &opts) {
        Ok(value) => {
            let (outcome, output) = emit(cli, &opts, &value);
            if let Some(out) = output {
                print!("{out}");
            }
            outcome
        }
        Err(diags) => render_diags(&diags),
    }
}

/// Lint pipeline: load → options → lint config → static analysis → emit.
///
/// Unlike compile mode, lint never reads stdin or creates temp directories,
/// and it never installs a command executor or IPC host. `lint_project`
/// handles static mode internally.
fn run_lint(cli: &ParsedCli) -> RunOutcome {
    let project = match load_project(&cli.path) {
        Ok(project) => project,
        Err(diags) => return render_diags_lint_failure(&diags),
    };

    let overrides = cli.overrides();
    let opts = match extract_options(&project, &overrides) {
        Ok(opts) => opts,
        Err(diags) => return render_diags_lint_failure(&diags),
    };

    let lint_overrides = LintOverrides {
        entry: overrides.entry.clone(),
        deny: cli.deny.clone(),
        allow: cli.allow.clone(),
        fail_on: cli.fail_on,
    };
    let lint_cfg = match extract_lint_config(&project, &lint_overrides) {
        Ok(cfg) => cfg,
        Err(diags) => return render_diags_lint_failure(&diags),
    };

    let report = lint_project(&project, &opts, &lint_cfg);
    let rendered = if let Some(Format::Json) = &cli.format {
        match render_lint_json(&report) {
            Ok(json) => json,
            Err(e) => {
                eprintln!("ymx: failed to serialize lint report: {e}");
                return RunOutcome::LintFailure;
            }
        }
    } else {
        render_lint_diagnostics(&report)
    };
    print!("{rendered}");

    if should_fail(&report, &lint_cfg) {
        RunOutcome::LintFailure
    } else {
        RunOutcome::Success
    }
}

/// Recursively discover `.yml`/`.yaml` files under `dir` (max depth 10).
/// Each file is its own project (its parent dir is the project root).
/// Skips files/directories matching `.gitignore`.
fn find_project_roots(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(10))
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext == "yml" || ext == "yaml" {
                results.push(path.to_path_buf());
            }
        }
    }

    results.sort();
    results
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

/// Handle extraction error for recursive tests, updating counters and printing results.
struct TestLine {
    passed: bool,
    output: String,
}

fn handle_extract_error(
    relpath: &str,
    diags: &[Diagnostic],
    expected_code: Option<&str>,
    total_tests: &mut usize,
    total_passed: &mut usize,
    overall_success: &mut bool,
) -> Option<TestLine> {
    if let Some(code) = expected_code {
        if diags.iter().any(|d| d.code == code) {
            *total_tests += 1;
            *total_passed += 1;
            Some(TestLine {
                passed: true,
                output: format!("PASS {}: _build_error", relpath),
            })
        } else {
            let actual_code = diags
                .first()
                .map(|d| d.code.to_string())
                .unwrap_or_default();
            *total_tests += 1;
            *overall_success = false;
            Some(TestLine {
                passed: false,
                output: format!(
                    "FAIL {}: _build_error mismatch (expected {}, got {})",
                    relpath, code, actual_code
                ),
            })
        }
    } else {
        eprintln!("ymx: warning: {}: {}", relpath, diags[0].message);
        None
    }
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
    let mut lines_to_print: Vec<TestLine> = Vec::new();

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
                        total_tests += 1;
                        total_passed += 1;
                        lines_to_print.push(TestLine {
                            passed: true,
                            output: format!("PASS {}: _build_error", relpath),
                        });
                    } else {
                        let actual_code = diags
                            .first()
                            .map(|d| d.code.to_string())
                            .unwrap_or_default();
                        total_tests += 1;
                        overall_success = false;
                        lines_to_print.push(TestLine {
                            passed: false,
                            output: format!(
                                "FAIL {}: _build_error mismatch (expected {}, got {})",
                                relpath, expected_code, actual_code
                            ),
                        });
                    }
                } else {
                    eprintln!("ymx: warning: {}: {}", relpath, diags[0].message);
                }
                continue;
            }
        };

        // For flat `<scenario>.yml` files the file IS the project root, so
        // try `main.main` first. If that fails with E009 (entry file not found),
        // fall back to `file_stem.main`.
        let file_stem = entry_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main");
        let entry = "main.main".to_string();
        let overrides = CliOverrides {
            entry: Some(entry.clone()),
            ..CliOverrides::default_for_tests()
        };

        let mut opts = match extract_options(&project, &overrides) {
            Ok(o) => o,
            Err(diags) => {
                // If main.main failed with E009, try file_stem.main as fallback
                let is_e009 = diags.iter().any(|d| d.code == "E009");
                if is_e009 && entry == "main.main" {
                    let fallback_entry = format!("{}.main", file_stem);
                    let fallback_overrides = CliOverrides {
                        entry: Some(fallback_entry.clone()),
                        ..CliOverrides::default_for_tests()
                    };
                    match extract_options(&project, &fallback_overrides) {
                        Ok(o) => o,
                        Err(diags) => {
                            if let Some(line) = handle_extract_error(
                                &relpath,
                                &diags,
                                build_error.as_deref(),
                                &mut total_tests,
                                &mut total_passed,
                                &mut overall_success,
                            ) {
                                lines_to_print.push(line);
                            }
                            continue;
                        }
                    }
                } else {
                    if let Some(line) = handle_extract_error(
                        &relpath,
                        &diags,
                        build_error.as_deref(),
                        &mut total_tests,
                        &mut total_passed,
                        &mut overall_success,
                    ) {
                        lines_to_print.push(line);
                    }
                    continue;
                }
            }
        };

        // Enable shell execution for tests
        opts.executor = Some(Arc::new(StdExecutor));

        // Inject StdIpcHost so IPC works in tests.
        if opts.ipc.is_none() {
            if let Some(ref exec) = opts.executor {
                opts.ipc = Some(Arc::new(StdIpcHost::new(exec.clone())));
            }
        }

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
                lines_to_print.push(TestLine {
                    passed: true,
                    output: format!("PASS {}: {}", relpath, result.test.target),
                });
            } else {
                overall_success = false;
                lines_to_print.push(TestLine {
                    passed: false,
                    output: format!("FAIL {}: {} {}", relpath, result.test.target, diff(result)),
                });
            }
        }
    }

    let any_failure = lines_to_print.iter().any(|l| !l.passed);

    for line in &lines_to_print {
        if any_failure && line.passed {
            continue;
        }
        println!("{}", line.output);
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
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;

    if failed > 0 {
        for result in &results {
            if !result.passed {
                println!("FAIL {} {}", result.test.target, diff(result));
            }
        }
    } else {
        for result in &results {
            if result.passed {
                println!("PASS {}", result.test.target);
            }
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
///
/// Returns `(RunOutcome, Option<String>)` where the `String` is stdout
/// content to emit (for watch-mode clearing), or `None` for file output or
/// diagnostics format.
fn emit(cli: &ParsedCli, opts: &Options, value: &Value) -> (RunOutcome, Option<String>) {
    match opts.format {
        Format::Diagnostics => (RunOutcome::Success, None),
        // JSON format uses pretty by default (milestone 1.23: "JSON output should
        // use `--pretty` by default"); the CLI --pretty flag overrides for compact
        Format::Json => emit_json(cli, true, value),
        // Compact format uses opts.pretty (false by default, true if --pretty given)
        Format::Compact => emit_json(cli, opts.pretty, value),
        // HTML format renders the value tree to HTML via DefaultHtmlRenderer.
        Format::Html => emit_html(cli, value),
        // PDF format renders the value tree to HTML then converts to PDF.
        Format::Pdf => emit_pdf(cli, opts, value),
    }
}

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

fn serialize(value: &Value, pretty: bool) -> Result<String, String> {
    if pretty {
        serde_json::to_string_pretty(value).map_err(|e| format!("failed to serialize JSON: {e}"))
    } else {
        serde_json::to_string(value).map_err(|e| format!("failed to serialize JSON: {e}"))
    }
}

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

/// Render diagnostics that abort a lint run. Lint load/option failures exit
/// `1`, not the compile mode's `2`.
fn render_diags_lint_failure(diags: &[Diagnostic]) -> RunOutcome {
    for diag in diags {
        eprintln!("{}", render_with_guidance(diag));
    }
    RunOutcome::LintFailure
}

/// Serialize `value` to JSON (pretty iff `pretty`) and dispatch to
/// `--output` or stdout. Serialization is materialized into a `String`
/// before any I/O, so a serialize failure never creates a file.
fn emit_json(cli: &ParsedCli, pretty: bool, value: &Value) -> (RunOutcome, Option<String>) {
    let json = match serialize(value, pretty) {
        Ok(json) => json,
        Err(message) => {
            eprintln!("ymx: {message}");
            return (RunOutcome::Diagnostic, None);
        }
    };
    match cli.output.as_deref() {
        Some(path) => (write_file(path, &json), None),
        None => (RunOutcome::Success, Some(json)),
    }
}

/// Render `value` to HTML via [`DefaultHtmlRenderer`] and dispatch to
/// `--output` or stdout.
fn emit_html(cli: &ParsedCli, value: &Value) -> (RunOutcome, Option<String>) {
    let html = DefaultHtmlRenderer.render_html(value);
    let output = if cli.pretty.unwrap_or(false) {
        pretty_print_html(&html)
    } else {
        html
    };
    match cli.output.as_deref() {
        Some(path) => (write_file(path, &output), None),
        None => (RunOutcome::Success, Some(output)),
    }
}

/// Render `value` to PDF via [`DefaultHtmlRenderer`] + ironpress and
/// dispatch binary output to `--output` or stdout.
fn emit_pdf(cli: &ParsedCli, _opts: &Options, value: &Value) -> (RunOutcome, Option<String>) {
    let html = DefaultHtmlRenderer.render_html(value);

    let backend = IronpressBackend;
    let pdf_bytes = backend.render(&html);

    if let Ok(bytes) = pdf_bytes {
        match cli.output.as_deref() {
            Some(path) => (write_file_binary(path, &bytes), None),
            None => {
                if let Err(e) = std::io::stdout().lock().write_all(&bytes) {
                    eprintln!("ymx: failed to write PDF to stdout: {e}");
                    (RunOutcome::Diagnostic, None)
                } else {
                    (RunOutcome::Success, None)
                }
            }
        }
    } else if let Err(e) = pdf_bytes {
        eprintln!("ymx: PDF render error: {}", e.message);
        (RunOutcome::Diagnostic, None)
    } else {
        unreachable!()
    }
}
