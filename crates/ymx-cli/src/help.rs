//! `--help` / `-h` manual page (milestone 1.10, task 6).
//!
//! A plain-text manual page printed to stdout and followed by exit `0`. Lists
//! every CLI flag (the full task-1 surface) with its default, the
//! `--plain` / `--plain-template` mutual exclusion, the `--output`
//! "written only on success" rule, and the exit-code contract. Kept as a
//! single [`MANUAL`] string so the binary's Help arm and any standalone
//! renderer share one source of truth.

/// The `ymx` manual page. Rendered verbatim to stdout on `--help` / `-h`.
pub const MANUAL: &str = "\
ymx 0.1.0
=====

NAME
    ymx — YAML → JSON compiler for the YMX language (v1).

SYNOPSIS
    ymx <file> [flags]

DESCRIPTION
    Compiles the YMX entry file <file> into JSON by resolving the entry component
    through the rules resolver, or runs the file's inline `_test` cases under
    --test. The project root is derived as <file>'s parent directory. Flag
    defaults come from the engine, overridable by the entry file's `_ymx` front
    matter, and overridable again by CLI flags (CLI > entry-file _ymx > engine
    default).

USAGE
    ymx <file> [flags]
        <file> is the entry file to compile (positional, required). The project
        root is derived as <file>'s parent directory.

    ymx --help | -h
        Print this manual page to stdout and exit 0.

FLAGS
    --entry <component>       Component name within <file> to compile (default: main.main =
                              component main in the entry file). The entry path internally
                              is <file_stem>.<component> (always exactly 2 segments). If the
                              file defines both <stem>.yml and <stem>.yaml the entry is
                              ambiguous (E009). If the file is missing or does not define
                              the component, the CLI emits E009 and exits non-zero.

    --from-keyword <kw>       Override the `from` keyword (default: from).

    --default-keyword <kw>    Override the `$default` keyword name (default: default);
                              the engine always prefixes the name with `$` internally.

    --max-depth <n>           Limit on template/call recursion (default: 256). <n> is
                              parsed as a non-negative u32; a non-integer is a usage
                              error (exit 2, no load).

    --pretty                  Pretty-print the JSON output (default: compact). Only
                              meaningful with --format json.

    --format <json|diagnostics>
                              Output style (default: json).
                                json         Serialize the resolved Value as JSON.
                                diagnostics  On a successful compile, emit nothing to
                                             stdout and exit 0; on any diagnostic,
                                             render to stderr and exit non-zero.

    --output <file>           Write JSON to <file> instead of stdout (default: stdout).
                              The file is written ONLY on success — if any diagnostic
                              is produced during load/extract/compile, the CLI exits
                              non-zero WITHOUT creating the file. Ignored under --test
                              and under --format diagnostics. A write failure prints a
                              diagnostic-style error to stderr, removes any partial
                              file (best effort), and exits non-zero.

    --plain                   Promote sub-namespace components AND templates into the
                              global namespace (equivalent to _ymx.plain: true).
                              Default: false (no promotion).

    --plain-template          Promote sub-namespace TEMPLATES ONLY into the global
                              namespace (equivalent to _ymx.plain: template).
                              Default: false (no promotion).
                              --plain and --plain-template are mutually exclusive:
                              providing both is a usage error (exit 2, NO load is
                              attempted). Each overrides the entry-file _ymx.plain
                              value per the precedence rule; a promoted name clashing
                              with an existing global name is E004.

    --test                    Run inline `_test` cases (via ymx-test) instead of
                              compiling the entry. Emits one line per test (PASS/FAIL
                              + a brief diff on failure) and exits non-zero if any test
                              fails OR any `_test` block fails to parse (E010). No JSON
                              is emitted under --test. Flag defaults still come from
                              the entry file's _ymx front matter. A project with no
                              _test blocks is a no-op success (exit 0).

    --help, -h                Print this manual page to stdout and exit 0.

EXIT CODES
    0   Success — compile succeeded (json or diagnostics), or --test with every
        test passing (including the no-_test-blocks no-op case), or --help / -h.
    1   A runtime diagnostic or test failure — load error (E001/E004/E007/E015),
        entry/options error (E009/E010), a malformed `_test` block (E010), a
        failing test under --test, or any error during compile
        (E002/E003/E005/E006/E008/E010/E011/E012/E013). Diagnostics are
        rendered to stderr as `[code] file:line:col (component): message`.
    2   Usage error — missing or extra positional path, bad --max-depth, bad
        --format, unknown flag, a flag missing its value, or --plain together
        with --plain-template. Printed to stderr as `ymx: <message>`; no
        load is attempted.

DIAGNOSTICS
    Every runtime diagnostic renders to stderr as
        [code] file:line:col (component): message
    where `file` is the resolved host-file path (rendered as `<?>` when no
    document is implicated, e.g. malformed entry path) and `component` is the
    name implicated (rendered as `<?>` when none).
";

/// Render the manual page. Currently identical to [`MANUAL`] (kept as a fn
/// in case a future caller wants formatting hooks without touching the
/// constant).
pub fn manual() -> &'static str {
    MANUAL
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manual must list every long flag and the three exit codes. This
    /// is the in-crate content guard; the binary-level `--help` wiring is
    /// exercised by `tests/cli.rs`.
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

    #[test]
    fn manual_lists_every_flag() {
        for flag in EXPECTED_FLAGS {
            assert!(MANUAL.contains(flag), "manual missing flag `{flag}`");
        }
    }

    #[test]
    fn manual_calls_out_mutual_exclusion() {
        assert!(
            MANUAL.contains("mutually exclusive"),
            "manual must call out --plain/--plain-template mutual exclusion"
        );
    }

    #[test]
    fn manual_states_output_written_only_on_success() {
        assert!(
            MANUAL.contains("written ONLY on success")
                || MANUAL.contains("written only on success"),
            "manual must state the --output success-only rule"
        );
    }

    #[test]
    fn manual_documents_exit_codes() {
        for marker in [
            "EXIT CODES",
            "0   Success",
            "1   A runtime",
            "2   Usage error",
        ] {
            assert!(MANUAL.contains(marker), "manual missing `{marker}`");
        }
    }

    #[test]
    fn manual_documents_defaults() {
        assert!(MANUAL.contains("default: main.main"));
        assert!(MANUAL.contains("default: from"));
        assert!(MANUAL.contains("default: default"));
        assert!(MANUAL.contains("default: 256"));
        assert!(MANUAL.contains("default: json"));
        assert!(MANUAL.contains("default: stdout"));
    }

    #[test]
    fn manual_fn_returns_same_string_as_const() {
        assert_eq!(manual(), MANUAL);
    }
}
