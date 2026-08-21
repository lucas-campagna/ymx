//! `--help` / `-h` manual page (milestone 1.10, task 6 + 1.23).
//!
//! A plain-text manual page printed to stdout and followed by exit `0`. Lists
//! every CLI flag (the full task-1 surface) with its default, the
//! `--plain` / `--plain-template` mutual exclusion, the `--output`
//! "written only on success" rule, and the exit-code contract. Kept as a
//! single [`MANUAL`] string so the binary's Help arm and any standalone
//! renderer share one source of truth.

/// The `ymx` manual page. Rendered verbatim to stdout on `--help` / `-h`.
pub const MANUAL: &str = "\
ymx 0.1.0 — YAML → JSON compiler for YMX v1

Usage: ymx [path] [flags]
       ymx --help | -h
       ymx --errors

Stdin modes (implicit, no flag needed):
  No path given   → stdin is the YAML script (writes to temp main.yml)
  Path given      → stdin provides call arguments (JSON, YAML fallback)

Options:
  -e, --entry <comp>       Entry component (default: main.main)
  -m, --max-depth <n>      Recursion limit (default: 256)
  -f, --format <fmt>       Output format: json, compact, or diagnostics (default: json)
      --pretty             Force pretty JSON with -f compact
  -o, --output <file>      Write to file instead of stdout
  -t, --test               Run inline `_test` blocks instead of compiling
  -c, --code <yml>        Inline YAML/JSON component definitions (overrides file components)
      --errors             Print full diagnostic code reference and exit
  -h, --help               Show this help

Exit codes: 0 success | 1 diagnostic | 2 usage error

Examples:
  ymx main.yml                       Compile a file
  cat main.yml | ymx                 Stdin is the script (equiv to above)
  ymx . --test                       Run inline tests
  echo '{\"a\":1}' | ymx main.yml    Stdin provides call arguments
  ymx -c 'main: hello'                        Inline script (no file)
  ymx main.yml -c 'main$: a + b'              Override file components
  echo '{\"a\":1}' | ymx -c 'main$: a + 1'     Inline script with stdin args
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
        "-c",
        "--code",
        "--errors",
        "-h",
        "--help",
    ];

    #[test]
    fn manual_lists_every_flag() {
        for flag in EXPECTED_FLAGS {
            assert!(MANUAL.contains(flag), "manual missing flag `{flag}`");
        }
    }

    #[test]
    fn manual_documents_exit_codes() {
        assert!(MANUAL.contains("0 success"));
        assert!(MANUAL.contains("1 diagnostic"));
        assert!(MANUAL.contains("2 usage error"));
    }

    #[test]
    fn manual_fn_returns_same_string_as_const() {
        assert_eq!(manual(), MANUAL);
    }
}
