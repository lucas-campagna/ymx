//! Loaded-project and compiler-options types.
//!
//! These are `ymx-core`'s public compile-time configuration surface,
//! re-exported by `ymx-lib`.

use std::path::PathBuf;

/// Output format selection for a compiled entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Format {
    Json,
    Diagnostics,
}

/// Namespace-promotion mode for `_ymx.plain` / CLI `--plain` /
/// `--plain-template`.
///
/// Per PRD, the YAML/CLI string maps to: `"false"` -> [`False`], `"true"` ->
/// [`All`] (promote components **and** templates), `"template"` ->
/// [`TemplatesOnly`] (promote templates only). An invalid value in the entry
/// file's `_ymx` is `E010`. Parsing the string into this enum is
/// `ymx-config`'s responsibility (milestone 1.4).
///
/// [`False`]: PlainMode::False
/// [`All`]: PlainMode::All
/// [`TemplatesOnly`]: PlainMode::TemplatesOnly
#[derive(Debug, Clone, PartialEq)]
pub enum PlainMode {
    False,
    All,
    TemplatesOnly,
}

/// Compiler options consumed by `compile` / `compile_component`.
///
/// Field defaults (PRD / AGENTS invariants): `entry = "main.main"` (a
/// file-path entry address, not a bare name), `from_keyword = "from"`,
/// `default_keyword = "default"`, `max_depth = 256`, `pretty = false`,
/// `format = Format::Json`, `plain = PlainMode::False`. Effective value
/// precedence is CLI flag > entry-file `_ymx` > engine default.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub entry: String,
    pub from_keyword: String,
    pub default_keyword: String,
    pub max_depth: u32,
    pub pretty: bool,
    pub format: Format,
    pub plain: PlainMode,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            entry: "main.main".to_string(),
            from_keyword: "from".to_string(),
            default_keyword: "default".to_string(),
            max_depth: 256,
            pretty: false,
            format: Format::Json,
            plain: PlainMode::False,
        }
    }
}

/// A loaded YMX project.
///
/// Per-document host file paths indexed by [`FileId`](crate::diag::FileId).
/// The merged component namespace, file-scoped components, and raw parsed
/// reserved meta-key (`_ymx`, `_test`) values arrive in milestone 1.3; this
/// milestone exposes only `files` as the stable public surface.
#[derive(Debug)]
pub struct Project {
    pub files: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_matches_engine_defaults() {
        let o = Options::default();
        assert_eq!(o.entry, "main.main");
        assert_eq!(o.from_keyword, "from");
        assert_eq!(o.default_keyword, "default");
        assert_eq!(o.max_depth, 256);
        assert!(!o.pretty);
        assert_eq!(o.format, Format::Json);
        assert_eq!(o.plain, PlainMode::False);
    }

    #[test]
    fn options_default_is_cloneable_and_comparable() {
        let a = Options::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn project_holds_files() {
        let p = Project {
            files: vec![PathBuf::from("main.yml")],
        };
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0], PathBuf::from("main.yml"));
    }
}
