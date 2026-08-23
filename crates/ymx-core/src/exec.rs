//! Shell-execution types: `ExecOutput`, `ExecError`, and the
//! [`CommandExecutor`] trait.
//!
//! This module is **I/O-free** — it defines the interface only.
//! Concrete implementations live in `ymx-lib` (`StdExecutor`) or
//! other I/O-capable crates.

use std::fmt;

/// Successful execution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Execution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// The backend name was not recognised by the executor.
    UnknownBackend(String),
    /// The command could not be spawned or failed at the OS level.
    SpawnFailed(String),
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::UnknownBackend(name) => write!(f, "unknown backend '{name}'"),
            ExecError::SpawnFailed(reason) => write!(f, "shell execution failed: {reason}"),
        }
    }
}

/// Trait for pluggable command execution backends.
///
/// Implementors must be `Send + Sync` so the executor can be stored in
/// [`Options`](crate::project::Options) and shared across threads.
/// `ymx-core` never calls this trait directly — the resolver calls it
/// only when evaluating `$<backend>{...}` value expressions.
pub trait CommandExecutor: Send + Sync + fmt::Debug {
    /// Execute `command` using the given `backend`.
    ///
    /// # Errors
    ///
    /// Returns [`ExecError::UnknownBackend`] if the backend is not
    /// recognised, or [`ExecError::SpawnFailed`] if the command could
    /// not be spawned.
    fn execute(&self, backend: &str, command: &str) -> Result<ExecOutput, ExecError>;
}
