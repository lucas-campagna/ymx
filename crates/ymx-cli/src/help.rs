//! `--help` / `-h` manual page (milestone 1.10, task 6 + 1.23).
//!
//! A plain-text manual page printed to stdout and followed by exit `0`. Lists
//! every CLI flag (the full task-1 surface) with its default, the
//! `--plain` / `--plain-template` mutual exclusion, the `--output`
//! "written only on success" rule, and the exit-code contract. Kept as a
//! single [`MAIN_MANUAL`] string so the binary's Help arm and any standalone
//! renderer share one source of truth.

/// The `ymx` manual page. Rendered verbatim to stdout on `--help` / `-h`.
pub const MAIN_MANUAL: &str = "\
ymx 0.1.0 — YAML → JSON compiler for YMX v1

Usage: ymx [path] [flags]
       ymx lint <path> [flags]
       ymx --help | -h
       ymx --errors

Stdin modes (implicit, no flag needed):
  No path given   → stdin is the YAML script (writes to temp main.yml)
  Path given      → stdin provides call arguments (JSON, YAML fallback)

Options:
  -e, --entry <comp>       Entry component within the file (default: main)
  -m, --max-depth <n>      Recursion limit (default: 256)
  -f, --format <fmt>       Output format: json, compact, or diagnostics (default: json)
      --pretty             Force pretty JSON with -f compact
  -o, --output <file>      Write to file instead of stdout
  -t, --test               Run inline `_test` blocks instead of compiling
      --watch <path>        Watch a file or directory for changes and re-compile on save
  -c, --code <yml>        Inline YAML/JSON component definitions (overrides file components)
      --errors             Print full diagnostic code reference and exit
  -h, --help               Show this help

Exit codes: 0 success | 1 diagnostic/lint failure | 2 usage error

Examples:
  ymx main.yml                       Compile a file
  cat main.yml | ymx                 Stdin is the script (equiv to above)
  ymx . --test                       Run inline tests
  echo '{\"a\":1}' | ymx main.yml    Stdin provides call arguments
  ymx -c 'main: hello'                        Inline script (no file)
  ymx main.yml -c 'main$: a + b'              Override file components
  ymx main.yml --watch .                      Watch the project and recompile on every change
  echo '{\"a\":1}' | ymx -c 'main$: a + 1'     Inline script with stdin args
";

/// The `ymx lint` manual page. Includes man page reference.
pub const LINT_MANUAL: &str = "\
ymx lint 0.1.0 — YMX linter

Usage: ymx lint <path> [flags]

Lint mode (never reads stdin):
  --deny <codes>       Repeatable/comma-separated L-codes to emit as errors
  --allow <codes>      Repeatable/comma-separated L-codes to suppress
  --fail-on <level>    Fail on error|warning|info|none (default: error)
  --format <fmt>       Lint output: json or diagnostics (default: diagnostics)
  Lint accepts --entry, --plain, --plain-template, --max-depth,
  --allowed-backends, and --allowed-ipc. -c/--code, --test, --watch,
  --output, --pretty, --errors, and --pdf-backend are usage errors.
  Lint config lives in the entry file's _ymx.lint block; L### findings are
  distinct from E### compiler diagnostics.

Exit codes: 0 success | 1 diagnostic/lint failure | 2 usage error

Examples:
  ymx lint main.yml                       Lint a file
  ymx lint .                              Lint current directory

See the full manual with `ymx --help` or the man page with `man ./man/ymx.1`.
";

/// Render the main manual page.
pub fn manual() -> &'static str {
    MAIN_MANUAL
}

/// Render the lint manual page.
pub fn lint_manual() -> &'static str {
    LINT_MANUAL
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manual must list every long flag and the three exit codes. This
    /// is the in-crate content guard; the binary-level `--help` wiring is
    /// exercised by `tests/cli.rs`.
    const EXPECTED_FLAGS: &[&str] = &[
        "-e",
        "--entry",
        "-m",
        "--max-depth",
        "-f",
        "--format",
        "--pretty",
        "-o",
        "--output",
        "-t",
        "--test",
        "--watch",
        "-c",
        "--code",
        "--errors",
        "-h",
        "--help",
    ];

    #[test]
    fn manual_lists_every_flag() {
        for flag in EXPECTED_FLAGS {
            assert!(
                MAIN_MANUAL.contains(flag),
                "main manual missing flag `{flag}`"
            );
        }
    }

    #[test]
    fn manual_documents_exit_codes() {
        assert!(MAIN_MANUAL.contains("0 success"));
        assert!(MAIN_MANUAL.contains("1 diagnostic"));
        assert!(MAIN_MANUAL.contains("2 usage error"));
    }

    #[test]
    fn manual_fn_returns_same_string_as_const() {
        assert_eq!(manual(), MAIN_MANUAL);
    }

    #[test]
    fn lint_manual_documents_exit_codes() {
        assert!(LINT_MANUAL.contains("0 success"));
        assert!(LINT_MANUAL.contains("1 diagnostic"));
        assert!(LINT_MANUAL.contains("2 usage error"));
    }

    #[test]
    fn lint_manual_contains_man_page_reference() {
        assert!(LINT_MANUAL.contains("man ./man/ymx.1"));
    }

    #[test]
    fn lint_manual_fn_returns_same_string_as_const() {
        assert_eq!(lint_manual(), LINT_MANUAL);
    }
}
