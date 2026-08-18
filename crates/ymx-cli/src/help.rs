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

Options:
  --entry <comp>       Entry component (default: main.main)
  --format <fmt>       Output format: json, compact, or diagnostics (default: json)
  --pretty             Force pretty JSON with --format compact
  --output <file>      Write to file instead of stdout
  --plain              Promote sub-namespace names to global (mutually exclusive
                       with --plain-template)
  --plain-template     Promote sub-namespace templates only (mutually exclusive
                       with --plain)
  --max-depth <n>      Recursion limit (default: 256)
  --test               Run inline `_test` blocks instead of compiling

Exit codes: 0 success | 1 diagnostic | 2 usage error
Examples: ymx main.yml | ymx . --test | ymx main.yml --entry foo
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
        "--max-depth",
        "--format",
        "--output",
        "--plain",
        "--plain-template",
        "--test",
        "--help",
        "-h",
        "--pretty",
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
