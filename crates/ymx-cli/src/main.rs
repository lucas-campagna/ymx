//! `ymx` — the YMX CLI binary.
//!
//! Thin entry point: parses `argv`, surfaces usage errors (exit 2), prints
//! `--help`, and delegates to [`run::run`] for the full pipeline:
//! `load_project` → `extract_options` → `compile` / `run_tests` → emit.
//!
//! ## Exit codes
//!
//! - `0` — success (no diagnostics)
//! - `1` — one or more diagnostics (compile error, test failure)
//! - `2` — CLI usage error (bad flags, missing file argument)

mod args;
mod help;
mod run;

use std::process::ExitCode;

use args::{parse, ParseOutcome};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        Ok(ParseOutcome::Help) => {
            print!("{}", help::manual());
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Cli(cli)) => run::run(&cli).to_exit_code(),
        Err(e) => {
            eprintln!("ymx: {message}", message = e.message);
            ExitCode::from(2)
        }
    }
}
