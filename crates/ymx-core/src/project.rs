//! Loaded-project and compiler-options types.
//!
//! These are `ymx-core`'s public compile-time configuration surface,
//! re-exported by `ymx-lib`.

use std::path::PathBuf;

use crate::diag::FileId;
use crate::ir::Value;
use crate::namespace::{FileScopeStore, NamespaceStore};

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
/// [`files`] is the stable `FileId`-indexable surface (host-file path per
/// document). [`namespaces`] holds the merged global + sub-namespaces
/// (non-`_`-prefixed definitions). [`file_scoped`] holds `_`-prefixed
/// definitions per document (excluded from the namespace merge; cross-document
/// references raise `E005` at the call site in milestone 1.6).
/// [`raw_meta_ymx`](Project::raw_meta_ymx) /
/// [`raw_meta_test`](Project::raw_meta_test) hold the parsed-but-uninterpreted
/// values of the reserved meta keys, keyed by [`FileId`] (lexicographic load
/// order). `ymx-core` only recognizes the two names, strips them from the
/// namespace, and stores their raw values; `ymx-config` interprets `_ymx`
/// (milestone 1.4) and `ymx-test` interprets `_test` (milestone 1.9). Per
/// invariant #4, non-entry `_ymx` blocks are never validated at load time.
///
/// [`files`]: Project::files
/// [`namespaces`]: Project::namespaces
/// [`file_scoped`]: Project::file_scoped
#[derive(Debug, Default)]
pub struct Project {
    /// `files[FileId.0]` — host-file path of every loaded document.
    pub files: Vec<PathBuf>,
    /// Merged global (`""`) + sub-namespaces (dotted relative path) of
    /// non-`_`-prefixed definitions.
    pub namespaces: NamespaceStore,
    /// `_`-prefixed (file-scoped) definitions, per [`FileId`].
    pub file_scoped: FileScopeStore,
    /// Raw parsed values of the `_ymx` meta key, one per document that
    /// declared it, in lexicographic load order. Uninterpreted at this layer.
    pub raw_meta_ymx: Vec<(FileId, Value)>,
    /// Raw parsed values of the `_test` meta key, one per document that
    /// declared it, in lexicographic load order. Uninterpreted at this layer.
    pub raw_meta_test: Vec<(FileId, Value)>,
}

impl Project {
    /// Empty project (no files, no definitions).
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` iff no document declared `_ymx`.
    pub fn has_no_ymx(&self) -> bool {
        self.raw_meta_ymx.is_empty()
    }

    /// `true` iff no document declared `_test`.
    pub fn has_no_test(&self) -> bool {
        self.raw_meta_test.is_empty()
    }
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
            ..Project::default()
        };
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0], PathBuf::from("main.yml"));
    }

    #[test]
    fn project_default_is_empty() {
        let p = Project::default();
        assert!(p.files.is_empty());
        assert!(p.namespaces.is_empty());
        assert_eq!(p.file_scoped.file_count(), 0);
    }

    #[test]
    fn project_new_is_default() {
        let p = Project::new();
        assert!(p.files.is_empty());
    }

    #[test]
    fn project_default_has_no_raw_meta() {
        let p = Project::default();
        assert!(p.has_no_ymx());
        assert!(p.has_no_test());
        assert!(p.raw_meta_ymx.is_empty());
        assert!(p.raw_meta_test.is_empty());
    }
}
