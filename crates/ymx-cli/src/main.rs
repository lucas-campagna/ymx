//! `ymx` binary — milestone 1.10 orchestration glue.
//!
//! The binary is thin: parse argv (task 1), service `--help` (manual text in
//! [`help`], task 6), surface usage errors (exit `2`), and dispatch into
//! [`run::run`] (task 3) for the load → extract → compile / `run_tests`
//! pipeline. The success-emit shape (JSON pretty / `--output` /
//! `--format diagnostics`) lands in task 4.

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
