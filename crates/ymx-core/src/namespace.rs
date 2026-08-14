//! Namespace model: storing and validating top-level definitions.
//!
//! A YMX project's top-level keys are components/templates. Each definition
//! lives in the namespace of its hosting directory (root -> global; a
//! subdirectory -> a sub-namespace addressed by a dotted path like
//! `subdir` or `subdir.inner`). Definitions whose effective identifier starts
//! with `_` are *file-scoped*: kept per-file and excluded from the namespace
//! merge, so cross-document `_`-prefixed references cannot resolve (the call
//! site raises `E005` in milestone 1.6).
//!
//! This module is pure data + validation predicates: the I/O layer
//! (`ymx-lib::load_project`, milestone 1.3 task 4) drives [`classify`] over the
//! parsed per-file [`Node`](crate::parse::Node) trees and records the resulting
//! [`Diagnostic`]s. The classifier distinguishes:
//!
//! * regular components/templates (registered, file-scoped if `_`-prefixed);
//! * the bare meta keys `_ymx` / `_test` (consumed — no component registered);
//! * reserved builtin names (`map` / `reduce` / `merge`) -> `E007` regardless
//!   of leading `$` count;
//! * leading-`$` variants of `_ymx` / `_test` (`$_ymx`, `$$test`, …) -> `E015`.
//!
//! Duplicate names within the same namespace (or same file for file-scoped
//! defs) are `E004`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::diag::{Diagnostic, FileId, Span, E004, E007, E015};
use crate::parse::Node;

/// The two reserved meta keys (bare form, consumed by the engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaKey {
    /// `_ymx` — front matter (interpreted by `ymx-config`).
    Ymx,
    /// `_test` — inline tests (interpreted by `ymx-test`).
    Test,
}

/// A registered top-level component/template definition.
#[derive(Debug, Clone)]
pub struct Definition {
    /// The hosting document.
    pub file: FileId,
    /// The full name as written (`a`, `$box`, `$$a`, …), including any leading
    /// `$`s. This is the namespace-store key.
    pub full_name: String,
    /// Span of the *key* (the component name location), for diagnostics.
    pub span: Span,
    /// The raw parsed body (pre-interpolation); the resolver (milestone 1.6)
    /// walks this into a [`crate::ir::Value`].
    pub body: Node,
}

/// Parsed metadata about a regular component/template name.
#[derive(Debug, Clone)]
pub struct ComponentMeta {
    /// The full name as written, including leading `$`s.
    pub full_name: String,
    /// Number of leading `$` characters. `0` -> regular component; `>=1` ->
    /// template at chain depth `dollar_count`.
    pub dollar_count: u32,
    /// The effective identifier (after stripping leading `$`s).
    pub effective_id: String,
    /// `true` iff the effective identifier starts with `_` (file-scoped).
    pub file_scoped: bool,
    /// Span of the name (for diagnostics).
    pub span: Span,
}

/// Classification of a top-level definition's name.
///
/// Produced by [`classify`]; the I/O layer consumes it to either register the
/// definition, consume the meta key (milestone 1.3 task 3), or emit a
/// load-time diagnostic.
#[derive(Debug, Clone)]
pub enum DefClass {
    /// A regular component/template. `file_scoped` selects the store.
    Component(ComponentMeta),
    /// The bare `_ymx` / `_test` meta key — consumed, never registered.
    MetaBare(MetaKey, Span),
    /// Reserved builtin name (`map` / `reduce` / `merge`) -> `E007`.
    BuiltinReserved(ComponentMeta),
    /// Leading-`$` variant of `_ymx` / `_test` (`$_ymx`, `$$test`, …) -> `E015`.
    MetaReserved(ComponentMeta),
    /// The name does not parse as a valid effective identifier (with optional
    /// leading `$`s) — e.g. contains a `.`, starts with a digit, or is empty.
    /// The I/O layer decides the diagnostic.
    InvalidName(Span),
}

impl DefClass {
    /// `true` if this classification is a regular component/template (i.e. the
    /// I/O layer should register it in a namespace or file-scope store).
    pub fn is_component(&self) -> bool {
        matches!(self, DefClass::Component(_))
    }

    /// `true` if this classification is the bare meta key to be consumed.
    pub fn is_meta_bare(&self) -> bool {
        matches!(self, DefClass::MetaBare(..))
    }

    /// Render the load-time diagnostic for a rejected class, attaching the
    /// resolved host-file path. Returns `None` for non-rejected classes
    /// ([`Component`](DefClass::Component) / [`MetaBare`](DefClass::MetaBare));
    /// [`InvalidName`](DefClass::InvalidName) also returns `None` here — the
    /// I/O layer renders it separately.
    pub fn into_diagnostic(self, file: PathBuf) -> Option<Diagnostic> {
        match self {
            DefClass::BuiltinReserved(meta) => Some(meta.reserved_builtin_diagnostic(file)),
            DefClass::MetaReserved(meta) => Some(meta.reserved_meta_diagnostic(file)),
            DefClass::Component(_) | DefClass::MetaBare(..) | DefClass::InvalidName(_) => None,
        }
    }
}

impl ComponentMeta {
    fn reserved_builtin_diagnostic(self, file: PathBuf) -> Diagnostic {
        Diagnostic {
            file: Some(file),
            line: self.span.line,
            col: self.span.col,
            component: Some(self.full_name.clone()),
            code: E007,
            message: format!(
                "reserved builtin name `{}` cannot be defined as a component or template",
                self.effective_id
            ),
        }
    }

    fn reserved_meta_diagnostic(self, file: PathBuf) -> Diagnostic {
        Diagnostic {
            file: Some(file),
            line: self.span.line,
            col: self.span.col,
            component: Some(self.full_name.clone()),
            code: E015,
            message: format!(
                "reserved meta-key variant `{}` (leading `$` on `_ymx`/`_test`) cannot be defined as a component or template",
                self.full_name
            ),
        }
    }
}

/// Classify a top-level definition name (already extracted from its YAML key).
///
/// The name is `[$]*<effective-id>` where the effective id is
/// `[A-Za-z_][A-Za-z0-9_]*`. The bare meta keys `_ymx` / `_test` are recognized
/// as consumed metadata (not components); their leading-`$` variants are
/// `E015`; the builtin effective ids `map` / `reduce` / `merge` are `E007` (any
/// leading `$` count).
pub fn classify(full_name: &str, span: Span) -> DefClass {
    let bytes = full_name.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx] == b'$' {
        idx += 1;
    }
    let dollar_count = idx as u32;
    let effective_id = &full_name[idx..];
    if !is_valid_effective_id(effective_id) {
        return DefClass::InvalidName(span);
    }
    let effective_id = effective_id.to_string();
    let file_scoped = effective_id.starts_with('_');
    let meta = ComponentMeta {
        full_name: full_name.to_string(),
        dollar_count,
        effective_id: effective_id.clone(),
        file_scoped,
        span,
    };
    match effective_id.as_str() {
        "map" | "reduce" | "merge" => DefClass::BuiltinReserved(meta),
        "_ymx" if dollar_count == 0 => DefClass::MetaBare(MetaKey::Ymx, span),
        "_test" if dollar_count == 0 => DefClass::MetaBare(MetaKey::Test, span),
        "_ymx" | "_test" => DefClass::MetaReserved(meta),
        _ => DefClass::Component(meta),
    }
}

/// `true` iff `id` matches the effective-identifier grammar
/// `[A-Za-z_][A-Za-z0-9_]*` (non-empty).
pub fn is_valid_effective_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A duplicate-name error from [`NamespaceStore::register`] (code [`E004`]).
#[derive(Debug, Clone)]
pub struct DupError {
    /// Span of the *second* (duplicate) definition's key.
    pub span: Span,
    /// Namespace dotted path (`""` for global), or the file-scoped file id
    /// path (rendered by the caller).
    pub namespace: String,
    /// The full name that collided.
    pub name: String,
}

impl DupError {
    /// Attach the resolved host-file path and stamp code [`E004`]. The
    /// component is the duplicated name.
    pub fn into_diagnostic(self, file: PathBuf) -> Diagnostic {
        Diagnostic {
            file: Some(file),
            line: self.span.line,
            col: self.span.col,
            component: Some(self.name.clone()),
            code: E004,
            message: format!(
                "duplicate component `{}` in namespace `{}`",
                self.name, self.namespace
            ),
        }
    }
}

/// A single namespace: a set of non-`_`-prefixed definitions keyed by full name.
#[derive(Debug, Clone, Default)]
pub struct Namespace {
    /// Dotted path (`""` for global).
    pub path: String,
    defs: HashMap<String, Definition>,
}

impl Namespace {
    /// Dotted namespace path (`""` for global).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Iterate over `(full_name, definition)` pairs in arbitrary order.
    pub fn defs(&self) -> impl Iterator<Item = (&str, &Definition)> {
        self.defs.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Look up a definition by full name.
    pub fn get(&self, full_name: &str) -> Option<&Definition> {
        self.defs.get(full_name)
    }

    /// Number of definitions in this namespace.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// `true` if this namespace holds no definitions.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// The merged namespace store: global namespace (`""`) plus one sub-namespace
/// per subdirectory (dotted relative path). Only non-`_`-prefixed definitions
/// live here; `_`-prefixed definitions are file-scoped (see
/// [`Project::file_scoped`](crate::project::Project::file_scoped)).
#[derive(Debug, Clone, Default)]
pub struct NamespaceStore {
    by_path: HashMap<String, Namespace>,
}

impl NamespaceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up `full_name` in namespace `path` (`""` for global).
    pub fn get(&self, path: &str, full_name: &str) -> Option<&Definition> {
        self.by_path.get(path).and_then(|ns| ns.get(full_name))
    }

    /// Borrow a namespace by dotted path.
    pub fn namespace(&self, path: &str) -> Option<&Namespace> {
        self.by_path.get(path)
    }

    /// Iterate over all namespaces `(dotted_path, namespace)` in arbitrary
    /// order.
    pub fn namespaces(&self) -> impl Iterator<Item = (&str, &Namespace)> {
        self.by_path.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of namespaces (global + sub).
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// `true` iff there are no namespaces at all (not even the global one).
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// Register `def` in namespace `path` (`""` for global). Returns
    /// [`DupError`] (code [`E004`]) if `def.full_name` is already present in
    /// that namespace; the duplicate is *not* inserted (the first definition
    /// wins).
    pub fn register(&mut self, path: &str, def: Definition) -> Result<(), DupError> {
        let ns = self
            .by_path
            .entry(path.to_string())
            .or_insert_with(|| Namespace {
                path: path.to_string(),
                defs: HashMap::new(),
            });
        if ns.defs.contains_key(&def.full_name) {
            return Err(DupError {
                span: def.span,
                namespace: path.to_string(),
                name: def.full_name.clone(),
            });
        }
        ns.defs.insert(def.full_name.clone(), def);
        Ok(())
    }
}

/// A per-file store of `_`-prefixed (file-scoped) definitions. Keyed first by
/// [`FileId`], then by full name.
#[derive(Debug, Clone, Default)]
pub struct FileScopeStore {
    by_file: HashMap<FileId, HashMap<String, Definition>>,
}

impl FileScopeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a file-scoped definition in `file` by full name.
    pub fn get(&self, file: FileId, full_name: &str) -> Option<&Definition> {
        self.by_file.get(&file).and_then(|m| m.get(full_name))
    }

    /// Iterate `(FileId, full_name, definition)` triples in arbitrary order.
    pub fn defs(&self) -> impl Iterator<Item = (FileId, &str, &Definition)> {
        self.by_file
            .iter()
            .flat_map(|(fid, m)| m.iter().map(move |(k, v)| (*fid, k.as_str(), v)))
    }

    /// Register a file-scoped `def` in `file`. Returns [`DupError`] (code
    /// [`E004`]) if `def.full_name` is already present in `file`; the duplicate
    /// is not inserted.
    pub fn register(&mut self, file: FileId, def: Definition) -> Result<(), DupError> {
        let m = self.by_file.entry(file).or_default();
        if m.contains_key(&def.full_name) {
            return Err(DupError {
                span: def.span,
                namespace: format!("<file {}>", file.0),
                name: def.full_name.clone(),
            });
        }
        m.insert(def.full_name.clone(), def);
        Ok(())
    }

    /// Number of files carrying at least one file-scoped definition.
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Total number of file-scoped definitions across all files.
    pub fn total(&self) -> usize {
        self.by_file.values().map(|m| m.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::E001;

    const S: Span = Span { line: 1, col: 1 };

    fn match_builtin(c: &DefClass) -> bool {
        matches!(c, DefClass::BuiltinReserved(_))
    }
    fn match_meta_reserved(c: &DefClass) -> bool {
        matches!(c, DefClass::MetaReserved(_))
    }
    fn match_meta_bare(c: &DefClass, kind: MetaKey) -> bool {
        matches!(c, DefClass::MetaBare(k, _) if *k == kind)
    }

    #[test]
    fn classifies_regular_components_and_templates() {
        let c = classify("a", S);
        let DefClass::Component(m) = &c else {
            panic!("expected Component, got {c:?}");
        };
        assert_eq!(m.dollar_count, 0);
        assert_eq!(m.effective_id, "a");
        assert!(!m.file_scoped);

        let DefClass::Component(m) = classify("$box", S) else {
            panic!("template");
        };
        assert_eq!(m.dollar_count, 1);
        assert_eq!(m.effective_id, "box");
        assert!(!m.file_scoped);

        let DefClass::Component(m) = classify("$$$a", S) else {
            panic!("deep template");
        };
        assert_eq!(m.dollar_count, 3);
        assert_eq!(m.effective_id, "a");
    }

    #[test]
    fn leading_underscore_means_file_scoped() {
        let DefClass::Component(m) = classify("_a", S) else {
            panic!("_a is a component");
        };
        assert!(m.file_scoped);
        assert_eq!(m.effective_id, "_a");
        assert_eq!(m.dollar_count, 0);

        let DefClass::Component(m) = classify("$_a", S) else {
            panic!("$_a is a file-scoped template");
        };
        assert!(m.file_scoped);
        assert_eq!(m.dollar_count, 1);
        assert_eq!(m.effective_id, "_a");

        let DefClass::Component(m) = classify("$$_a", S) else {
            panic!("$$_a");
        };
        assert!(m.file_scoped);
        assert_eq!(m.dollar_count, 2);
    }

    #[test]
    fn builtin_reserved_name_is_e007_any_dollar_count() {
        assert!(match_builtin(&classify("map", S)));
        assert!(match_builtin(&classify("reduce", S)));
        assert!(match_builtin(&classify("merge", S)));
        assert!(match_builtin(&classify("$map", S)));
        assert!(match_builtin(&classify("$$reduce", S)));
        assert!(match_builtin(&classify("$$$merge", S)));
        assert_eq!(
            classify("map", S)
                .into_diagnostic(PathBuf::from("f.yml"))
                .unwrap()
                .code,
            E007
        );
    }

    #[test]
    fn underscore_prefixed_non_reserved_is_not_builtin() {
        // `_map` is file-scoped, not a reserved builtin.
        let DefClass::Component(m) = classify("_map", S) else {
            panic!("_map is a file-scoped component");
        };
        assert!(m.file_scoped);
        assert_eq!(m.effective_id, "_map");
    }

    #[test]
    fn near_builtins_are_ordinary_components() {
        let DefClass::Component(m) = classify("mapping", S) else {
            panic!("mapping is ordinary");
        };
        assert_eq!(m.effective_id, "mapping");
        let DefClass::Component(m) = classify("reducer", S) else {
            panic!("reducer is ordinary");
        };
        assert_eq!(m.effective_id, "reducer");
    }

    #[test]
    fn bare_meta_keys_are_consumed_not_e015() {
        assert!(match_meta_bare(&classify("_ymx", S), MetaKey::Ymx));
        assert!(match_meta_bare(&classify("_test", S), MetaKey::Test));
        // Bare meta keys are not errors.
        assert!(classify("_ymx", S)
            .into_diagnostic(PathBuf::from("f.yml"))
            .is_none());
        assert!(classify("_test", S)
            .into_diagnostic(PathBuf::from("f.yml"))
            .is_none());
        // Bare meta keys are *not* file-scoped components either.
        assert!(!classify("_ymx", S).is_component());
    }

    #[test]
    fn leading_dollar_meta_variants_are_e015() {
        // Reading: E015 iff the effective identifier (all leading `$`s stripped)
        // is `_ymx` or `_test` AND there is ≥1 leading `$`. So `$_ymx`, `$_test`,
        // `$$_ymx`, `$$_test`, … are all E015; the bare `_ymx`/`_test` are
        // consumed (MetaBare), not E015.
        assert!(match_meta_reserved(&classify("$_ymx", S)));
        assert!(match_meta_reserved(&classify("$_test", S)));
        assert!(match_meta_reserved(&classify("$$_ymx", S)));
        assert!(match_meta_reserved(&classify("$$_test", S)));
        assert!(match_meta_reserved(&classify("$$$_ymx", S)));
        assert!(match_meta_reserved(&classify("$$$_test", S)));
        let diag = classify("$_ymx", S)
            .into_diagnostic(PathBuf::from("f.yml"))
            .unwrap();
        assert_eq!(diag.code, E015);
        assert_eq!(diag.file, Some(PathBuf::from("f.yml")));
        assert_eq!(diag.component.as_deref(), Some("$_ymx"));
        assert_eq!((diag.line, diag.col), (1, 1));
    }

    #[test]
    fn dollar_dollar_test_is_a_regular_component_under_rule_text() {
        // PRD §Reserved names lists `$$test` as an E015 example, but the *rule
        // text* ("effective identifier equals `_ymx` or `_test`") + the
        // effective-id grammar make `$$test` strip to effective id `test`
        // (the underscore is dropped), so it is a regular component, not E015.
        // The other PRD examples (`$_ymx`, `$$_ymx`) keep the underscore and
        // are E015. This test pins the rule-text reading; see the open question
        // reported to the orchestrator (proposed PRD diff: `$$test` -> `$_test`).
        let c = classify("$$test", S);
        let DefClass::Component(m) = &c else {
            panic!("$$test should be a regular component under the rule text, got {c:?}");
        };
        assert_eq!(m.dollar_count, 2);
        assert_eq!(m.effective_id, "test");
        assert!(!m.file_scoped);
        assert!(c.into_diagnostic(PathBuf::from("f.yml")).is_none());
    }

    #[test]
    fn invalid_names_are_flagged() {
        // contains a dot (namespace dots are lookup-only, not part of an id)
        assert!(matches!(classify("a.b", S), DefClass::InvalidName(_)));
        // starts with a digit
        assert!(matches!(classify("9a", S), DefClass::InvalidName(_)));
        // empty effective id (only `$`s)
        assert!(matches!(classify("$$", S), DefClass::InvalidName(_)));
        // empty name
        assert!(matches!(classify("", S), DefClass::InvalidName(_)));
        // Valid names are not InvalidName.
        assert!(!matches!(classify("$a", S), DefClass::InvalidName(_)));
    }

    #[test]
    fn effective_id_grammar() {
        assert!(is_valid_effective_id("a"));
        assert!(is_valid_effective_id("_a"));
        assert!(is_valid_effective_id("_"));
        assert!(is_valid_effective_id("a1_b"));
        assert!(is_valid_effective_id("A_B2"));
        assert!(!is_valid_effective_id(""));
        assert!(!is_valid_effective_id("9"));
        assert!(!is_valid_effective_id("a.b"));
        assert!(!is_valid_effective_id("a-b"));
        assert!(!is_valid_effective_id("a b"));
    }

    fn def(name: &str, file: u32, line: u32) -> Definition {
        Definition {
            file: FileId(file),
            full_name: name.to_string(),
            span: Span { line, col: 1 },
            body: Node::Null(Span { line, col: 1 }),
        }
    }

    #[test]
    fn namespace_register_and_lookup() {
        let mut store = NamespaceStore::new();
        assert!(store.is_empty());
        store
            .register("", def("a", 0, 1))
            .expect("register global a");
        store
            .register("", def("$a", 0, 2))
            .expect("register global $a (distinct from a)");
        store
            .register("subdir", def("box", 1, 3))
            .expect("register subdir box");

        assert!(store.get("", "a").is_some());
        assert!(store.get("", "$a").is_some());
        assert!(store.get("", "b").is_none());
        assert!(store.get("subdir", "box").is_some());
        assert!(store.get("subdir", "a").is_none(), "global a not in subdir");
        assert!(store.get("nonexistent", "x").is_none());
        assert_eq!(store.len(), 2, "global + subdir");
        assert_eq!(store.namespace("").unwrap().len(), 2);
        assert_eq!(store.namespace("subdir").unwrap().len(), 1);
    }

    #[test]
    fn namespace_duplicate_in_same_namespace_is_e004() {
        let mut store = NamespaceStore::new();
        store.register("", def("a", 0, 1)).expect("first a");
        let err = store.register("", def("a", 2, 5)).unwrap_err();
        let diag = err.into_diagnostic(PathBuf::from("other.yml"));
        assert_eq!(diag.code, E004);
        assert_eq!(diag.component.as_deref(), Some("a"));
        assert_eq!(diag.file, Some(PathBuf::from("other.yml")));
        // The duplicate's span (line 5) is reported, not the first's (line 1).
        assert_eq!((diag.line, diag.col), (5, 1));
        // The first definition stays; the duplicate was not inserted.
        assert!(store.get("", "a").is_some());
        assert_eq!(store.namespace("").unwrap().len(), 1);
    }

    #[test]
    fn same_name_in_different_namespaces_is_not_e004() {
        let mut store = NamespaceStore::new();
        store.register("", def("a", 0, 1)).expect("global a");
        store
            .register("subdir", def("a", 1, 2))
            .expect("subdir a (distinct namespace)");
        assert_eq!(store.len(), 2);
        assert!(store.get("", "a").is_some());
        assert!(store.get("subdir", "a").is_some());
    }

    #[test]
    fn file_scope_store_keeps_underscore_defs_per_file() {
        let mut fs = FileScopeStore::new();
        fs.register(FileId(0), def("_a", 0, 1)).expect("file0 _a");
        fs.register(FileId(0), def("$_a", 0, 2))
            .expect("file0 $_a (distinct)");
        // same name in a different file is fine — file scope is per-document.
        fs.register(FileId(1), def("_a", 1, 3))
            .expect("file1 _a (different file)");

        assert!(fs.get(FileId(0), "_a").is_some());
        assert!(fs.get(FileId(0), "$_a").is_some());
        assert!(fs.get(FileId(0), "a").is_none(), "global a not file-scoped");
        assert!(fs.get(FileId(1), "_a").is_some());
        assert_eq!(fs.file_count(), 2);
        assert_eq!(fs.total(), 3);

        // duplicate _a in the same file is E004.
        let err = fs.register(FileId(0), def("_a", 0, 9)).unwrap_err();
        let diag = err.into_diagnostic(PathBuf::from("f0.yml"));
        assert_eq!(diag.code, E004);
        assert_eq!((diag.line, diag.col), (9, 1));
    }

    #[test]
    fn file_scoped_defs_do_not_participate_in_namespace_merge() {
        // Putting `_a` into the global namespace store should be a separate
        // decision; here we verify the classifier routes it to file-scope, not
        // to the namespace. The loader (task 4) consults `file_scoped` for the
        // underscore branch.
        let c = classify("_a", S);
        let DefClass::Component(m) = &c else {
            panic!();
        };
        assert!(m.file_scoped);
        // A namespaced lookup for `_a` finds nothing (only non-_ defs there).
        let mut store = NamespaceStore::new();
        store
            .register("", def("a", 0, 1))
            .expect("global a registered");
        assert!(
            store.get("", "_a").is_none(),
            "file-scoped _a is excluded from the namespace"
        );
        let _ = E001; // ensure the unused-import-style reference is exercised.
    }
}
