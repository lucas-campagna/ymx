//! Argument parser for the `ymx` CLI (milestones 1.10 task 1 + task 2).
//!
//! Hand-rolled — `clap` is not a workspace dependency today, and a small
//! parser keeps the dependency tree minimal. Recognises the flag surface in
//! PRD §CLI:
//!   `ymx <path> [flags]`
//!   -e, --entry <path>  -m, --max-depth <n>   --pretty
//!   -f, --format <json|diagnostics>
//!   -o, --output <file>
//!   -t, --test           -h, --help
//!
//! Every flag maps to `None` when absent, so the [`ParsedCli`] is a 1:1 source
//! for `ymx_config::CliOverrides` via [`ParsedCli::overrides`]. The
//! orchestration-only concerns (`path`, `output`, `test`) stay on
//! [`ParsedCli`] and never reach `ymx_config::CliOverrides`.

use std::io::IsTerminal;
use std::path::PathBuf;

use ymx_config::CliOverrides;
use ymx_lib::ymx_core::project::Format;

/// The parsed command line: the file positional and per-flag inputs
/// (`None` when the flag was absent — i.e. defer to `_ymx` then engine
/// default), plus the CLI-only orchestration concerns (`output`, `test`)
/// that do not flow into `ymx_config::CliOverrides`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCli {
    /// `ymx <file>` — the entry file. Always present (parse errors
    /// otherwise). The project root is derived as `path.parent()`.
    pub path: PathBuf,
    /// `--entry <component>` (default `main`). The bare component name within
    /// the entry file; the entry path internally is `<file_stem>.<component>`
    /// (always exactly 2 segments).
    pub entry: Option<String>,
    /// `--max-depth <n>` (default `256`). Parsed as `u32`; non-integers
    /// error.
    pub max_depth: Option<u32>,
    /// `--pretty` (default `false`). The flag is a switch, so this is
    /// `Some(true)` when present and `None` when absent.
    pub pretty: Option<bool>,
    /// `--format <json|diagnostics>` (default `json`).
    pub format: Option<Format>,
    /// `--output <file>` orchestration concern — never consumed by
    /// `extract_options`.
    pub output: Option<PathBuf>,
    /// `--test` orchestration concern — runs `_test` blocks instead of
    /// compiling the entry.
    pub test: bool,
    /// Set when `--test` is given and the path resolved to a directory at
    /// parse time (via [`Path::is_dir`](std::path::Path::is_dir)). `None`
    /// when `--test` is absent or when the path was a file (or does not exist).
    /// Used by `run.rs` to decide between single-project and recursive modes.
    pub test_dir: Option<PathBuf>,
    /// True when no positional was given and stdin is the script (non-test
    /// mode only). In this mode `run.rs` reads stdin as a YAML document,
    /// writes it to a temp file, and compiles it. False when a positional
    /// file path was provided (stdin is args, if non-tty, in non-test mode).
    pub stdin_is_script: bool,
}

impl ParsedCli {
    /// Build a [`CliOverrides`] from the parsed per-flag inputs — the
    /// CLI-overrides shape consumed by `ymx_config::extract_options`.
    ///
    /// Each flag maps to its `Option` field verbatim: `None` when the flag
    /// was absent (deferring to the entry-file `_ymx`, then the engine
    /// default per the PRD's precedence ladder), `Some(value)` when present.
    /// The CLI-only orchestration concerns (`path`, `output`, `test`) are
    /// deliberately **not** part of `CliOverrides` — `extract_options` does
    /// not consume them.
    // Called only by the orchestration pipeline (task 3); tests reach it via
    // `ParsedCli::overrides`. `dead_code` allowed until task 3 wires it.
    #[allow(dead_code)]
    pub fn overrides(&self) -> CliOverrides {
        let file_stem = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("main");
        let component = self.entry.as_deref().unwrap_or("main");
        let entry = format!("{}.{}", file_stem, component);
        CliOverrides {
            entry: Some(entry),
            max_depth: self.max_depth,
            pretty: self.pretty,
            format: self.format.clone(),
            plain: None,
            allowed_backends: None,
        }
    }
}

/// A usage error: the message to print to stderr. The caller prints
/// `ymx: <message>` and exits non-zero — the binary uses exit code `2` for
/// usage errors so they remain distinguishable from a runtime diagnostic's
/// exit `1`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
}

/// Parser outcome: a fully parsed command line, or a `--help` / `-h`
/// request (serviced by the caller — printing the manual and exiting `0`).
#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome {
    Cli(ParsedCli),
    Help,
}

/// Parse `ymx <path> [flags]`.
///
/// Pass `args = &argv[1..]` (the program name is not consumed). The parser
/// walks left-to-right, recognising flags in any order interspersed with the
/// single required positional. `--flag value` is the supported form
/// (`--flag=value` is rejected — keep it simple).
pub fn parse(args: &[String]) -> Result<ParseOutcome, ParseError> {
    let mut positionals: Vec<PathBuf> = Vec::new();
    let mut entry: Option<String> = None;
    let mut max_depth: Option<u32> = None;
    let mut pretty: Option<bool> = None;
    let mut format: Option<Format> = None;
    let mut output: Option<PathBuf> = None;
    let mut test = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" => return Ok(ParseOutcome::Help),
            "-t" | "--test" => test = true,
            "--pretty" => pretty = Some(true),
            "-e" | "--entry" => entry = Some(take_value(args, &mut i, "--entry")?),
            "-m" | "--max-depth" => {
                let raw = take_value(args, &mut i, "--max-depth")?;
                let n: u32 = raw.parse().map_err(|_| ParseError {
                    message: format!("--max-depth: `{raw}` is not a non-negative integer"),
                })?;
                max_depth = Some(n);
            }
            "-f" | "--format" => {
                let raw = take_value(args, &mut i, "--format")?;
                format = Some(match raw.as_str() {
                    "json" => Format::Json,
                    "compact" => Format::Compact,
                    "diagnostics" => Format::Diagnostics,
                    other => {
                        return Err(ParseError {
                            message: format!(
                                "--format: `{other}` is not `json`, `compact`, or `diagnostics`"
                            ),
                        })
                    }
                });
            }
            "-o" | "--output" => {
                let raw = take_value(args, &mut i, "--output")?;
                output = Some(PathBuf::from(raw));
            }
            other => {
                if other.starts_with("--") {
                    return Err(ParseError {
                        message: format!("unknown flag `{other}`"),
                    });
                }
                if other.starts_with('-') && other.len() > 1 {
                    return Err(ParseError {
                        message: format!("unknown short option `{other}`"),
                    });
                }
                positionals.push(PathBuf::from(other));
            }
        }
        i += 1;
    }

    // When --test is given, 0 positionals is acceptable (defaults to ".")
    // When --test is NOT given, 0 positionals means stdin-as-script (if non-tty).
    // 2+ positionals is always an error.
    if positionals.len() > 1 {
        return Err(ParseError {
            message: format!(
                "expected exactly one file, got {} — usage: `ymx <file> [flags]`",
                positionals.len()
            ),
        });
    }

    let stdin_is_script: bool;
    let path: PathBuf;

    if positionals.is_empty() {
        if test {
            // --test with no positional: use "." as project dir; no stdin involved.
            stdin_is_script = false;
            path = PathBuf::from(".");
        } else {
            // Non-test with no positional: stdin is the script.
            if std::io::stdin().is_terminal() {
                return Err(ParseError {
                    message: "stdin is a terminal, cannot read script or args".to_string(),
                });
            }
            stdin_is_script = true;
            path = PathBuf::from("."); // sentinel; run.rs replaces with temp file
        }
    } else {
        // One positional given: run.rs reads stdin as args only if non-tty.
        // If stdin is a tty (no data piped), run.rs compiles without args.
        stdin_is_script = false;
        path = positionals.pop().unwrap();
    }

    // Determine test_dir: set when --test is given and the path is a directory.
    // When stdin_is_script is true the actual project path is a temp file created
    // at run time, so test_dir must be None even if the sentinel path is ".".
    let test_dir = if !stdin_is_script && path.is_dir() {
        Some(path.clone())
    } else {
        None
    };
    Ok(ParseOutcome::Cli(ParsedCli {
        path,
        entry,
        max_depth,
        pretty,
        format,
        output,
        test,
        test_dir,
        stdin_is_script,
    }))
}

/// Advance `i` past the flag's name and return the next argument (the
/// flag's value). Errors if the value is absent (the flag was last on the
/// command line).
fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, ParseError> {
    if *i + 1 >= args.len() {
        return Err(ParseError {
            message: format!("{flag}: missing value"),
        });
    }
    *i += 1;
    Ok(args[*i].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn cli_of(parts: &[&str]) -> ParsedCli {
        match parse(&args(parts)) {
            Ok(ParseOutcome::Cli(c)) => c,
            other => panic!("expected Cli, got {other:?}"),
        }
    }

    fn err_of(parts: &[&str]) -> ParseError {
        parse(&args(parts)).expect_err("expected usage error")
    }

    #[test]
    fn bare_path_yields_none_overrides() {
        let c = cli_of(&["proj/main.yml"]);
        assert_eq!(c.path, PathBuf::from("proj/main.yml"));
        assert_eq!(c.entry, None);
        assert_eq!(c.max_depth, None);
        assert_eq!(c.pretty, None);
        assert_eq!(c.format, None);
        assert_eq!(c.output, None);
        assert!(!c.test);
    }

    #[test]
    fn entry_parses_bare_component() {
        let c = cli_of(&["--entry", "foo", "proj/main.yml"]);
        assert_eq!(c.entry.as_deref(), Some("foo"));
    }

    #[test]
    fn keyword_flags_are_not_cli_flags() {
        let err = err_of(&["--from-keyword", "frm", "proj/main.yml"]);
        assert!(err.message.contains("unknown flag"));

        let err = err_of(&["--default-keyword", "dflt", "proj/main.yml"]);
        assert!(err.message.contains("unknown flag"));
    }

    #[test]
    fn max_depth_parses_u32() {
        let c = cli_of(&["--max-depth", "8", "proj/main.yml"]);
        assert_eq!(c.max_depth, Some(8));
        assert_eq!(
            cli_of(&["--max-depth", "0", "proj/main.yml"]).max_depth,
            Some(0)
        );

        let err = err_of(&["--max-depth", "abc", "proj/main.yml"]);
        assert!(err.message.contains("--max-depth"));
        assert!(err.message.contains("abc"));

        let err = err_of(&["--max-depth", "-1", "proj/main.yml"]);
        assert!(err.message.contains("--max-depth"));

        let err = err_of(&["--max-depth", "99999999999999999999", "proj/main.yml"]);
        assert!(err.message.contains("--max-depth"));
    }

    #[test]
    fn pretty_flag_only_sets_true() {
        let c = cli_of(&["--pretty", "proj/main.yml"]);
        assert_eq!(c.pretty, Some(true));

        assert_eq!(cli_of(&["proj/main.yml"]).pretty, None);
    }

    #[test]
    fn format_parses_json_compact_and_diagnostics() {
        let c = cli_of(&["--format", "json", "proj/main.yml"]);
        assert_eq!(c.format, Some(Format::Json));

        let c = cli_of(&["--format", "compact", "proj/main.yml"]);
        assert_eq!(c.format, Some(Format::Compact));

        let c = cli_of(&["--format", "diagnostics", "proj/main.yml"]);
        assert_eq!(c.format, Some(Format::Diagnostics));

        let err = err_of(&["--format", "xml", "proj/main.yml"]);
        assert!(err.message.contains("xml"));
    }

    #[test]
    fn output_parses_path() {
        let c = cli_of(&["--output", "out.json", "proj/main.yml"]);
        assert_eq!(c.output.as_deref(), Some(std::path::Path::new("out.json")));
    }

    #[test]
    fn test_flag_flips() {
        let c = cli_of(&["--test", "proj/main.yml"]);
        assert!(c.test);

        assert!(!cli_of(&["proj/main.yml"]).test);
    }

    #[test]
    fn help_short_and_long_request_help() {
        assert_eq!(parse(&args(&["-h"])), Ok(ParseOutcome::Help));
        assert_eq!(parse(&args(&["--help"])), Ok(ParseOutcome::Help));
        assert_eq!(
            parse(&args(&["--help", "proj/main.yml"])),
            Ok(ParseOutcome::Help)
        );
        assert_eq!(
            parse(&args(&["proj/main.yml", "--test", "--help"])),
            Ok(ParseOutcome::Help)
        );
    }

    #[test]
    fn too_many_positionals_errors() {
        let err = err_of(&["proj/main.yml", "extra.yml"]);
        assert!(err.message.contains("exactly one"));
    }

    #[test]
    fn unknown_long_flag_errors() {
        let err = err_of(&["--bogus", "proj/main.yml"]);
        assert!(err.message.contains("unknown flag"));
        assert!(err.message.contains("--bogus"));
    }

    #[test]
    fn unknown_short_option_errors() {
        let err = err_of(&["-x", "proj/main.yml"]);
        assert!(err.message.contains("unknown short option"));
    }

    #[test]
    fn flag_without_value_errors() {
        let err = err_of(&["--entry"]);
        assert!(err.message.contains("--entry"));
        assert!(err.message.contains("missing value"));

        let err = err_of(&["proj/main.yml", "--max-depth"]);
        assert!(err.message.contains("missing value"));

        let err = err_of(&["proj/main.yml", "--output"]);
        assert!(err.message.contains("missing value"));
    }

    #[test]
    fn flags_interspersed_with_path() {
        let c = cli_of(&[
            "--pretty",
            "proj/main.yml",
            "--test",
            "--format",
            "diagnostics",
        ]);
        assert_eq!(c.path, PathBuf::from("proj/main.yml"));
        assert_eq!(c.pretty, Some(true));
        assert!(c.test);
        assert_eq!(c.format, Some(Format::Diagnostics));
    }

    // ---- task 2: ParsedCli::overrides() -> CliOverrides ----

    #[test]
    fn overrides_derives_entry_from_file_stem_and_default() {
        // cli_of(["proj/main.yml"]) with no --entry: entry derived as "main.main"
        let c = cli_of(&["proj/main.yml"]);
        let ov = c.overrides();
        assert_eq!(ov.entry.as_deref(), Some("main.main"));
        assert_eq!(ov.max_depth, None);
        assert_eq!(ov.pretty, None);
        assert_eq!(ov.format, None);
        assert_eq!(ov.plain, None);
    }

    #[test]
    fn overrides_maps_each_present_flag_field() {
        let c = cli_of(&[
            "--entry",
            "foo",
            "--max-depth",
            "8",
            "--pretty",
            "--format",
            "diagnostics",
            "proj/main.yml",
        ]);
        let ov = c.overrides();
        // entry = file_stem.component = "main.foo"
        assert_eq!(ov.entry.as_deref(), Some("main.foo"));
        assert_eq!(ov.max_depth, Some(8));
        assert_eq!(ov.pretty, Some(true));
        assert_eq!(ov.format, Some(Format::Diagnostics));
    }

    #[test]
    fn overrides_format_json_maps_to_json() {
        let c = cli_of(&["--format", "json", "proj/main.yml"]);
        assert_eq!(c.overrides().format, Some(Format::Json));
    }

    #[test]
    fn overrides_absent_flags_defer_to_none() {
        // Only --max-depth is present; all others None (including entry which
        // is always derived, not None in overrides).
        let c = cli_of(&["--max-depth", "100", "proj/main.yml"]);
        let ov = c.overrides();
        assert_eq!(ov.max_depth, Some(100));
        // entry is ALWAYS Some(...) from CLI — derived from file_stem.component
        assert_eq!(ov.entry.as_deref(), Some("main.main"));
        assert_eq!(ov.pretty, None);
        assert_eq!(ov.format, None);
        assert_eq!(ov.plain, None);
    }

    #[test]
    fn overrides_does_not_carry_output_or_test() {
        // `CliOverrides` has no `output`/`test` fields — the struct-literal
        // construction in `overrides()` is the real guard against a future
        // regression (adding such a field would fail to compile). Here we
        // confirm `--test` and `--output` stay on `ParsedCli` and do not
        // populate any override field.  entry is always Some(...) so we
        // compare field-by-field rather than equality with default_for_tests.
        let c = cli_of(&["--test", "--output", "out.json", "proj/main.yml"]);
        assert!(c.test);
        assert_eq!(c.output.as_deref(), Some(std::path::Path::new("out.json")));
        let ov = c.overrides();
        // entry is always derived (never None from CLI)
        assert_eq!(ov.entry.as_deref(), Some("main.main"));
        // output/test not carried
        assert_eq!(ov.max_depth, None);
        assert_eq!(ov.pretty, None);
        assert_eq!(ov.format, None);
        assert_eq!(ov.plain, None);
    }

    // ---- task 1: recursive test discovery ---

    #[test]
    fn test_flag_without_path_defaults_to_dot() {
        // `ymx --test` (no positional) should accept and default path to "."
        let c = cli_of(&["--test"]);
        assert_eq!(c.path, PathBuf::from("."));
        assert!(c.test);
        assert!(c.test_dir.is_some()); // "." is a directory
        assert_eq!(c.test_dir.unwrap(), PathBuf::from("."));
    }

    #[test]
    fn test_flag_with_nonexistent_dir_path_leaves_test_dir_none() {
        // When --test is given with a path that doesn't exist, is_dir() returns
        // false, so test_dir should be None (treat as file path, let load fail later)
        let c = cli_of(&["--test", "some_nonexistent_dir"]);
        assert!(c.test);
        assert!(c.test_dir.is_none()); // doesn't exist → not a directory
    }

    #[test]
    fn test_flag_with_file_leaves_test_dir_none() {
        // When --test is given with a file path, test_dir is None
        let c = cli_of(&["--test", "proj/main.yml"]);
        assert!(c.test);
        assert!(c.test_dir.is_none()); // file path, not directory
    }

    #[test]
    fn test_flag_with_dot_leaves_test_dir_some() {
        // `ymx --test .` → path is ".", which is a directory
        let c = cli_of(&["--test", "."]);
        assert!(c.test);
        assert_eq!(c.path, PathBuf::from("."));
        assert!(c.test_dir.is_some());
        assert_eq!(c.test_dir.unwrap(), PathBuf::from("."));
    }

    #[test]
    fn test_flag_with_extra_positional_still_errors() {
        // --test with 2 positionals should still error with "expected exactly one"
        let err = err_of(&["--test", "proj/main.yml", "extra.yml"]);
        assert!(err.message.contains("expected exactly one"));
    }

    #[test]
    fn non_test_empty_args_with_non_tty_stdin_succeeds_with_stdin_is_script() {
        // Without --test, empty args with non-tty stdin → stdin-is-script mode.
        // (stdin is never a tty in cargo test, so this succeeds.)
        let c = cli_of(&[]);
        assert!(c.stdin_is_script);
        assert_eq!(c.path, PathBuf::from("."));
        assert!(!c.test);
        assert!(c.test_dir.is_none()); // stdin-is-script overrides test_dir
    }

    #[test]
    fn positional_with_non_tty_stdin_sets_stdin_is_script_false() {
        // With a positional and non-tty stdin, stdin_is_script is false (stdin is args).
        let c = cli_of(&["proj/main.yml"]);
        assert!(!c.stdin_is_script);
        assert_eq!(c.path, PathBuf::from("proj/main.yml"));
    }

    #[test]
    fn test_flag_without_path_does_not_set_stdin_is_script() {
        // --test with no positional: path=".''; stdin_is_script must stay false.
        let c = cli_of(&["--test"]);
        assert!(!c.stdin_is_script);
        assert!(c.test);
        assert_eq!(c.path, PathBuf::from("."));
    }
}
