//! `ymx` binary — milestone 1.10 orchestration glue.
//!
//! Task 1 wires the arg parser: parse argv, service `--help`, and surface
//! usage errors. Compile/test orchestration (load → extract → compile /
//! `run_tests` → emit) and exit-code handling land in tasks 2–5; the manual
//! page text lands in task 6.

mod args;

use std::process::ExitCode;

use args::{parse, ParseOutcome};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        Ok(ParseOutcome::Help) => {
            // Manual page text lands in task 6.
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Cli(_cli)) => {
            // Orchestration wires in task 3; `CliOverrides` mapping is task 2.
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ymx: {message}", message = e.message);
            ExitCode::from(2)
        }
    }
}
