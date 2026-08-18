//! Loaded-project and compiler-options types.
//!
//! These are `ymx-core`'s public compile-time configuration surface,
//! re-exported by `ymx-lib`.

use std::path::PathBuf;

use crate::diag::FileId;
use crate::ir::Value;
use crate::namespace::{Definition, FileScopeStore, NamespaceStore};

/// Output format selection for a compiled entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Format {
    Json,
    Compact,
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
/// [`root`](Project::root) is the directory [`load_project`] walked;
/// [`files`] holds each document's root-joined path (so `FileId` indexes the
/// path, and `strip_prefix(root)` recovers the relative path for entry-path
/// resolution). [`namespaces`] holds the merged global + sub-namespaces
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
    /// The root directory walked by `ymx-lib::load_project`. `files` are
    /// root-joined paths.
    pub root: PathBuf,
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

/// The effective global namespace under a [`PlainMode`].
///
/// [`global`](EffectiveNamespace::global) lists the root namespace's
/// definitions; [`promoted`](EffectiveNamespace::promoted) lists the
/// sub-namespace definitions that `plain` promotes into the global namespace.
/// Promotion is deterministic: sub-namespaces are visited in lexicographic
/// dotted-path order and their definitions in lexicographic full-name order.
/// `PlainMode::False` promotes nothing; `PlainMode::All` promotes components
/// **and** templates; `PlainMode::TemplatesOnly` promotes only `$`-prefixed
/// templates. This mirrors [`resolve_ref`](crate::resolve::resolve_ref)'s
/// lookup-time promotion semantics (global wins; promoted names are consulted
/// in lexicographic order). `ymx-config` uses it for the extraction-time
/// `E004` promotion-clash check; the resolver (milestone 1.6) can use it for
/// bare-name lookups.
pub struct EffectiveNamespace<'a> {
    /// The global namespace's definitions, `(full_name, definition)`.
    pub global: Vec<(&'a str, &'a Definition)>,
    /// Promoted sub-namespace definitions,
    /// `(full_name, namespace_dotted_path, definition)` — the path lets
    /// callers render the qualified promoted name (e.g. `subdir.name`).
    pub promoted: Vec<(&'a str, &'a str, &'a Definition)>,
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

    /// The effective global namespace under `plain` (see
    /// [`EffectiveNamespace`]): the root namespace's definitions plus the
    /// sub-namespace definitions that `plain` promotes.
    pub fn effective_global_namespace(&self, plain: PlainMode) -> EffectiveNamespace<'_> {
        let mut global: Vec<(&str, &Definition)> = self
            .namespaces
            .namespace("")
            .map(|ns| ns.defs().collect())
            .unwrap_or_default();
        global.sort_unstable_by_key(|(a, _)| *a);
        if plain == PlainMode::False {
            return EffectiveNamespace {
                global,
                promoted: Vec::new(),
            };
        }
        let templates_only = plain == PlainMode::TemplatesOnly;
        let mut paths: Vec<&str> = self
            .namespaces
            .namespaces()
            .map(|(path, _)| path)
            .filter(|path| !path.is_empty())
            .collect();
        paths.sort_unstable();
        let mut promoted: Vec<(&str, &str, &Definition)> = Vec::new();
        for path in paths {
            let ns = self
                .namespaces
                .namespace(path)
                .expect("path came from namespaces()");
            let mut defs: Vec<(&str, &Definition)> = ns.defs().collect();
            defs.sort_unstable_by_key(|(a, _)| *a);
            for (name, def) in defs {
                if !templates_only || name.starts_with('$') {
                    promoted.push((name, path, def));
                }
            }
        }
        EffectiveNamespace { global, promoted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::parse::Node;

    const SPAN: Span = Span { line: 1, col: 1 };

    fn def(file: u32, name: &str) -> Definition {
        Definition {
            file: FileId(file),
            full_name: name.to_string(),
            span: SPAN,
            body: Node::Int(1, SPAN),
        }
    }

    /// Project rooted at `/proj`:
    /// * `main.yml`     (FileId 0): `main`, `$box`
    /// * `a/b.yml`      (FileId 1): `x`, `$xbox`
    /// * `subdir/t.yml` (FileId 2): `t`, `$tbox`, `x`
    fn project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![
            PathBuf::from("/proj/main.yml"),
            PathBuf::from("/proj/a/b.yml"),
            PathBuf::from("/proj/subdir/t.yml"),
        ];
        p.namespaces.register("", def(0, "main")).unwrap();
        p.namespaces.register("", def(0, "$box")).unwrap();
        p.namespaces.register("a", def(1, "x")).unwrap();
        p.namespaces.register("a", def(1, "$xbox")).unwrap();
        p.namespaces.register("subdir", def(2, "t")).unwrap();
        p.namespaces.register("subdir", def(2, "$tbox")).unwrap();
        p.namespaces.register("subdir", def(2, "x")).unwrap();
        p
    }

    #[test]
    fn effective_global_namespace_false_promotes_nothing() {
        let p = project();
        let view = p.effective_global_namespace(PlainMode::False);
        let names: Vec<&str> = view.global.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["$box", "main"], "global defs sorted by name");
        assert!(view.promoted.is_empty(), "False promotes nothing");
    }

    #[test]
    fn effective_global_namespace_all_promotes_components_and_templates() {
        let p = project();
        let view = p.effective_global_namespace(PlainMode::All);
        let names: Vec<&str> = view.global.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["$box", "main"]);
        let promoted: Vec<(&str, &str, u32)> = view
            .promoted
            .iter()
            .map(|(name, path, def)| (*name, *path, def.file.0))
            .collect();
        assert_eq!(
            promoted,
            [
                ("$xbox", "a", 1),
                ("x", "a", 1),
                ("$tbox", "subdir", 2),
                ("t", "subdir", 2),
                ("x", "subdir", 2),
            ],
            "lexicographic (path, name) order; components and templates both promoted"
        );
    }

    #[test]
    fn effective_global_namespace_templates_only_promotes_dollar_names() {
        let p = project();
        let view = p.effective_global_namespace(PlainMode::TemplatesOnly);
        let promoted: Vec<(&str, &str)> = view
            .promoted
            .iter()
            .map(|(name, path, _)| (*name, *path))
            .collect();
        assert_eq!(
            promoted,
            [("$xbox", "a"), ("$tbox", "subdir")],
            "only $ names are promoted under TemplatesOnly"
        );
    }

    #[test]
    fn effective_global_namespace_empty_project_promotes_nothing() {
        let p = Project::new();
        for plain in [PlainMode::False, PlainMode::All, PlainMode::TemplatesOnly] {
            let view = p.effective_global_namespace(plain.clone());
            assert!(view.global.is_empty(), "{plain:?}");
            assert!(view.promoted.is_empty(), "{plain:?}");
        }
    }

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
