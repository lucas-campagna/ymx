//! Front-matter config & entry-path resolution (milestone 1.4).
//!
//! [`extract_options`] applies, per flag, the precedence **CLI flag >
//! entry-file `_ymx` > engine default** and returns the effective
//! [`Options`]. The entry file — the document whose `_ymx` block is the
//! project's front matter — is located by the entry path (CLI `--entry` if
//! set, else the literal default `main.main`). Non-entry `_ymx` blocks are
//! completely ignored: never parsed or validated (invariant #4).
//!
//! `ymx-config` is I/O-free: it consumes an already-loaded [`Project`] and
//! never touches the filesystem.

use ymx_core::project::{Format, PlainMode};

/// Per-flag CLI override (`None` = flag not provided on the command line).
///
/// The CLI is the top of the per-flag precedence ladder (CLI > entry-file
/// `_ymx` > engine default); a `None` field defers to the entry file, then to
/// the engine default.
#[derive(Debug, Clone, PartialEq)]
pub struct CliOverrides {
    /// `--entry <path>` override (default `main.main`).
    pub entry: Option<String>,
    /// `--from-keyword <kw>` override (default `from`).
    pub from_keyword: Option<String>,
    /// `--default-keyword <kw>` override (default `default`; the engine
    /// prefixes `$` internally).
    pub default_keyword: Option<String>,
    /// `--max-depth <n>` override (default `256`).
    pub max_depth: Option<u32>,
    /// `--pretty` override (default `false`).
    pub pretty: Option<bool>,
    /// `--format <json|diagnostics>` override (default `Format::Json`).
    pub format: Option<Format>,
    /// `--plain` / `--plain-template` override (default
    /// `PlainMode::False`). `--plain` maps to `PlainMode::All`,
    /// `--plain-template` to `PlainMode::TemplatesOnly`.
    pub plain: Option<PlainMode>,
}

impl CliOverrides {
    /// All-`None` overrides — the harness shape used by `_test` runs (PRD
    /// §Testing: `extract_options(&project, &CliOverrides::default_for_tests())`).
    pub fn default_for_tests() -> Self {
        CliOverrides {
            entry: None,
            from_keyword: None,
            default_keyword: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
        }
    }
}
