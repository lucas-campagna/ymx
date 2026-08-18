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
use crate::ir::Value;
use crate::parse::{node_to_value, Entry, Key, Node};

/// The three reserved meta keys (bare form, consumed by the engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaKey {
    /// `_ymx` — front matter (interpreted by `ymx-config`).
    Ymx,
    /// `_test` — inline tests (interpreted by `ymx-test`).
    Test,
    /// `_use` — file imports (interpreted by `ymx-lib`).
    Use,
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
        "_use" if dollar_count == 0 => DefClass::MetaBare(MetaKey::Use, span),
        "_ymx" | "_test" | "_use" => DefClass::MetaReserved(meta),
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

/// A bare meta key (`_ymx` / `_test`) found at a document's top level, paired
/// with its raw parsed value (span-less [`Value`]) and defining [`FileId`].
///
/// Load-time meta extraction is *uninterpreted* (invariant #4): the value is
/// stored verbatim, whether or not it is a well-formed front-matter mapping or
/// `_test` block. Validation is `ymx-config`'s / `ymx-test`'s job (milestones
/// 1.4 / 1.9), applied to the entry file's `_ymx` and to `_test` blocks of
/// readable carriers only.
#[derive(Debug, Clone)]
pub struct MetaValue {
    pub key: MetaKey,
    pub file: FileId,
    pub value: Value,
}

/// Outcome of classifying the top-level entries of one document for the I/O
/// layer (task 4). Regular components/templates are returned as
/// [`Definition`]s for the caller to register; bare meta keys become
/// [`MetaValue`]s for the caller to append to
/// [`Project::raw_meta_ymx`](crate::project::Project::raw_meta_ymx) /
/// [`raw_meta_test`](crate::project::Project::raw_meta_test); rejected names
/// (`E007` builtin, `E015` meta-reserved) become yield diagnostics once the
/// caller attaches the file path.
#[derive(Debug, Default)]
pub struct DocExtract {
    /// Regular non-`_`-prefixed definitions to register in a namespace store.
    pub defs: Vec<Definition>,
    /// `_`-prefixed (file-scoped) definitions to register in a file-scope
    /// store (same [`FileId`] for all entries here).
    pub file_scoped_defs: Vec<Definition>,
    /// Bare `_ymx` meta values (at most one per document).
    pub meta_ymx: Option<MetaValue>,
    /// Bare `_test` meta values (at most one per document).
    pub meta_test: Option<MetaValue>,
    /// Bare `_use` meta values (at most one per document).
    pub meta_use: Option<MetaValue>,
    /// Classifications rejected at load time (`E007` / `E015`), awaiting the
    /// resolved file path to be rendered. The I/O layer folds these into the
    /// `Vec<Diagnostic>` returned by `load_project`.
    pub rejections: Vec<DefClass>,
}

/// Extract the per-document namespace + meta contributions from a parsed tree.
///
/// `file` is the [`FileId`] of the hosting document; `body` is its parsed
/// [`Node`] (typically a `Node::Object`, but a scalar/array top level — e.g. an
/// empty document parsed to `Node::Null` — yields no definitions and no
/// meta). The returned [`DocExtract`] is pure data; the I/O layer drives the
/// actual registration (calling [`NamespaceStore::register`] /
/// [`FileScopeStore::register`], which may surface `E004` duplicates) and the
/// `Vec<Diagnostic>` collection (attaching the host-file path to each
/// [`DefClass`] rejection).
///
/// Behavior:
/// * only a `Node::Object` contributes entries; any other top-level shape
///   contributes nothing;
/// * a non-`String` top-level key (e.g. an integer) is classified as
///   [`DefClass::InvalidName`] for the I/O layer to render — non-string keys
///   are not legal component/template names, and they are never meta keys;
/// * the bare `_ymx` / `_test` entries are consumed (never registered as
///   components); a document carrying both is fine;
/// * the value stored on a [`MetaValue`] is the span-less `Value` form (via
///   [`node_to_value`]), unvalidated — invariant #4;
/// * leading-`$` variants of `_ymx`/`_test` and builtin effective ids route to
///   `rejections` (they are **not** consumed as meta and **not** registered);
/// * a duplicate bare `_ymx` (or `_test`) in the same document is treated as a
///   second copy of the meta key and is not separately stored (the first wins);
///   the loader's namespace `E004` check does not apply to consumed meta keys
///   because they are never registered. The PRD does not define this case
///   further; we keep the first occurrence (rename one of them to register a
///   real component instead).
pub fn extract_document(file: FileId, body: &Node) -> DocExtract {
    let mut out = DocExtract::default();
    let entries = match body {
        Node::Object(entries, _) => entries,
        _ => return out,
    };
    for Entry {
        key,
        key_span,
        value,
    } in entries
    {
        let name = match key {
            Key::String(s) => s.as_str(),
            _ => {
                out.rejections.push(DefClass::InvalidName(*key_span));
                continue;
            }
        };
        match classify(name, *key_span) {
            DefClass::Component(meta) => {
                let def = Definition {
                    file,
                    full_name: meta.full_name.clone(),
                    span: meta.span,
                    body: value.clone(),
                };
                if meta.file_scoped {
                    out.file_scoped_defs.push(def);
                } else {
                    out.defs.push(def);
                }
            }
            DefClass::MetaBare(kind, _) => {
                let mv = MetaValue {
                    key: kind,
                    file,
                    value: node_to_value(value),
                };
                match kind {
                    MetaKey::Ymx if out.meta_ymx.is_none() => out.meta_ymx = Some(mv),
                    MetaKey::Test if out.meta_test.is_none() => out.meta_test = Some(mv),
                    MetaKey::Use if out.meta_use.is_none() => out.meta_use = Some(mv),
                    MetaKey::Ymx | MetaKey::Test | MetaKey::Use => {
                        // A duplicate bare meta key in the same document: the
                        // first wins; the second is silently dropped (it is
                        // not a component, and meta keys are not subject to
                        // the namespace `E004` check).
                    }
                }
            }
            reject => out.rejections.push(reject),
        }
    }
    out
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
        assert!(match_meta_bare(&classify("_use", S), MetaKey::Use));
        // Bare meta keys are not errors.
        assert!(classify("_ymx", S)
            .into_diagnostic(PathBuf::from("f.yml"))
            .is_none());
        assert!(classify("_test", S)
            .into_diagnostic(PathBuf::from("f.yml"))
            .is_none());
        assert!(classify("_use", S)
            .into_diagnostic(PathBuf::from("f.yml"))
            .is_none());
        // Bare meta keys are *not* file-scoped components either.
        assert!(!classify("_ymx", S).is_component());
        assert!(!classify("_test", S).is_component());
        assert!(!classify("_use", S).is_component());
    }

    #[test]
    fn leading_dollar_meta_variants_are_e015() {
        // Reading: E015 iff the effective identifier (all leading `$`s stripped)
        // is `_ymx` or `_test` or `_use` AND there is ≥1 leading `$`. So `$_ymx`,
        // `$_test`, `$_use`, `$$_ymx`, `$$_test`, `$$_use`, … are all E015; the
        // bare `_ymx`/`_test`/`_use` are consumed (MetaBare), not E015.
        assert!(match_meta_reserved(&classify("$_ymx", S)));
        assert!(match_meta_reserved(&classify("$_test", S)));
        assert!(match_meta_reserved(&classify("$_use", S)));
        assert!(match_meta_reserved(&classify("$$_ymx", S)));
        assert!(match_meta_reserved(&classify("$$_test", S)));
        assert!(match_meta_reserved(&classify("$$_use", S)));
        assert!(match_meta_reserved(&classify("$$$_ymx", S)));
        assert!(match_meta_reserved(&classify("$$$_test", S)));
        assert!(match_meta_reserved(&classify("$$$_use", S)));
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

    // ---- Task 3: meta-key handling ----

    fn extract(file: u32, src: &str) -> DocExtract {
        extract_document(
            FileId(file),
            &crate::parse::parse_document(src).expect("parse"),
        )
    }

    fn object_value(entries: &[(&str, Value)]) -> Value {
        let mut m = indexmap::IndexMap::new();
        for (k, v) in entries {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    }

    #[test]
    fn bare_ymx_is_consumed_not_registered() {
        let mut ex = extract(0, "_ymx:\n  max_depth: 100\nmain: 1\n");
        assert_eq!(ex.defs.len(), 1, "main is the only component");
        assert_eq!(ex.defs[0].full_name, "main");
        assert!(ex.file_scoped_defs.is_empty());
        assert!(ex.meta_ymx.is_some(), "_ymx consumed as meta");
        assert!(ex.meta_test.is_none());
        assert!(
            ex.rejections.is_empty(),
            "bare _ymx is not an error (invariant #4)"
        );
        let mv = ex.meta_ymx.take().unwrap();
        assert_eq!(mv.file, FileId(0));
        assert_eq!(mv.value, object_value(&[("max_depth", Value::Int(100))]));
    }

    #[test]
    fn bare_test_is_consumed_not_registered() {
        let mut ex = extract(0, "_test:\n  main: 42\nmain: 1\n");
        assert_eq!(ex.defs.len(), 1, "only main is a component");
        assert!(ex.meta_ymx.is_none());
        assert!(ex.meta_test.is_some(), "_test consumed as meta");
        assert!(ex.rejections.is_empty());
        let mv = ex.meta_test.take().unwrap();
        assert_eq!(mv.file, FileId(0));
        assert_eq!(mv.value, object_value(&[("main", Value::Int(42))]));
    }

    #[test]
    fn both_metas_in_same_file() {
        let mut ex = extract(0, "_ymx:\n  pretty: true\n_test:\n  main: 1\nmain: 5\n");
        assert_eq!(ex.defs.len(), 1, "main still registers normally");
        assert!(ex.meta_ymx.is_some());
        assert!(ex.meta_test.is_some());
        assert!(ex.rejections.is_empty());
        let ymx = ex.meta_ymx.take().unwrap();
        assert_eq!(ymx.value, object_value(&[("pretty", Value::Bool(true))]));
        let test = ex.meta_test.take().unwrap();
        assert_eq!(test.value, object_value(&[("main", Value::Int(1))]));
    }

    #[test]
    fn ymx_malformed_body_stored_verbatim_without_error() {
        // Invariant #4: a non-entry _ymx is never validated, and even an entry
        // _ymx is only validated by ymx-config (1.4). At load time we store the
        // raw value regardless of shape. Here `_ymx` is a mapping with an
        // unknown field + a wrong-typed field.
        let mut ex = extract(
            0,
            "_ymx:\n  unknown_field: hi\n  max_depth: not-a-number\nmain: 1\n",
        );
        assert!(ex.rejections.is_empty(), "no validation at load time");
        let mv = ex.meta_ymx.take().unwrap();
        assert_eq!(
            mv.value,
            object_value(&[
                ("unknown_field", Value::String("hi".into())),
                ("max_depth", Value::String("not-a-number".into())),
            ])
        );
    }

    #[test]
    fn ymx_non_mapping_value_stored_verbatim_without_error() {
        // `_ymx: 5` — a scalar meta value. Stored verbatim; not an error.
        let mut ex = extract(0, "_ymx: 5\nmain: 1\n");
        assert!(ex.rejections.is_empty());
        let mv = ex.meta_ymx.take().unwrap();
        assert_eq!(mv.value, Value::Int(5));
    }

    #[test]
    fn ymx_null_value_stored_verbatim() {
        // `_ymx:` (null) — a null meta value. Stored verbatim.
        let mut ex = extract(0, "_ymx:\nmain: 1\n");
        assert!(ex.rejections.is_empty());
        let mv = ex.meta_ymx.take().unwrap();
        assert_eq!(mv.value, Value::Null);
    }

    #[test]
    fn ymx_array_value_stored_verbatim() {
        let mut ex = extract(0, "_ymx:\n  - 1\n  - 2\nmain: 1\n");
        assert!(ex.rejections.is_empty());
        let mv = ex.meta_ymx.take().unwrap();
        assert_eq!(mv.value, Value::Array(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn dollar_ymx_variant_is_e015_not_consumed() {
        // `$_ymx` effective id `_ymx`, 1 leading $ -> E015. NOT stored as meta.
        let ex = extract(0, "$_ymx: 1\nmain: 2\n");
        assert_eq!(ex.defs.len(), 1, "main registers normally");
        assert!(ex.meta_ymx.is_none(), "$_ymx is NOT consumed as meta");
        assert_eq!(ex.rejections.len(), 1);
        let diag = ex
            .rejections
            .into_iter()
            .next()
            .unwrap()
            .into_diagnostic(PathBuf::from("f.yml"))
            .unwrap();
        assert_eq!(diag.code, E015);
        assert_eq!(diag.component.as_deref(), Some("$_ymx"));
    }

    #[test]
    fn dollar_dollar_ymx_and_dollar_test_variants_are_e015() {
        for name in ["$_test", "$$_ymx", "$$_test", "$$$_ymx", "$$$_test"] {
            let body = format!("{name}: 1\nmain: 2\n");
            let ex = extract(0, &body);
            assert!(ex.meta_ymx.is_none(), "{name} not consumed as _ymx");
            assert!(ex.meta_test.is_none(), "{name} not consumed as _test");
            assert_eq!(ex.rejections.len(), 1, "{name} should be E015");
            let class = &ex.rejections[0];
            assert!(match_meta_reserved(class), "{name} should be MetaReserved");
        }
    }

    #[test]
    fn dollar_dollar_test_is_regular_component_under_reading_a() {
        // `$$test` strips to effective id `test` (underscore dropped) -> a
        // regular component, NOT E015 and NOT meta. Pinned reading A.
        let ex = extract(0, "$$test: 1\nmain: 2\n");
        assert_eq!(ex.defs.len(), 2, "$$test registers as a component");
        let names: Vec<&str> = ex.defs.iter().map(|d| d.full_name.as_str()).collect();
        assert!(names.contains(&"$$test"));
        assert!(names.contains(&"main"));
        assert!(ex.meta_test.is_none());
        assert!(ex.meta_ymx.is_none());
        assert!(ex.rejections.is_empty());
    }

    #[test]
    fn non_meta_object_keys_register_alongside_meta() {
        // Siblings still register with their `$`-prefixes preserved; templates
        // and file-scoped defs coexist with meta keys.
        let ex = extract(0, "_ymx:\n  pretty: true\na: 1\n$box: 2\n_b: 3\n");
        assert_eq!(ex.defs.len(), 2, "a + $box in namespace");
        assert_eq!(ex.file_scoped_defs.len(), 1, "_b file-scoped");
        let names: Vec<&str> = ex.defs.iter().map(|d| d.full_name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"$box"));
        assert_eq!(ex.file_scoped_defs[0].full_name, "_b");
        assert!(ex.meta_ymx.is_some());
    }

    #[test]
    fn non_string_top_level_key_is_invalid_name() {
        // An integer top-level key is not a legal component name; it is also
        // never a meta key. The I/O layer renders it as a diagnostic.
        let ex = extract(0, "0: a\nmain: 1\n");
        assert_eq!(ex.defs.len(), 1, "only main registers");
        assert_eq!(ex.rejections.len(), 1);
        assert!(matches!(ex.rejections[0], DefClass::InvalidName(_)));
    }

    #[test]
    fn empty_document_yields_no_meta_and_no_defs() {
        let ex = extract(0, "");
        assert!(ex.defs.is_empty());
        assert!(ex.file_scoped_defs.is_empty());
        assert!(ex.meta_ymx.is_none());
        assert!(ex.meta_test.is_none());
        assert!(ex.rejections.is_empty());
    }

    #[test]
    fn non_object_top_level_yields_nothing() {
        let ex = extract(0, "- 1\n- 2\n");
        assert!(ex.meta_ymx.is_none());
        assert!(ex.meta_test.is_none());
        assert!(ex.defs.is_empty());
        assert!(ex.rejections.is_empty());
    }

    #[test]
    fn duplicate_bare_meta_in_same_document_first_wins() {
        // The PRD does not define a duplicate bare meta key; we keep the first
        // and drop the second silently (not E004 — meta keys are never
        // registered in the namespace store). One meta value per kind max.
        let mut ex = extract(
            0,
            "_ymx:\n  pretty: true\n_ymx:\n  pretty: false\nmain: 1\n",
        );
        assert!(ex.meta_ymx.is_some());
        let mv = ex.meta_ymx.take().unwrap();
        assert_eq!(mv.value, object_value(&[("pretty", Value::Bool(true))]));
        assert!(ex.rejections.is_empty());
        assert_eq!(ex.defs.len(), 1, "main registers normally");
    }

    #[test]
    fn bare_use_is_consumed_not_registered() {
        let mut ex = extract(0, "_use:\n  \"*\": foo\nmain: 1\n");
        assert_eq!(ex.defs.len(), 1, "main is the only component");
        assert!(ex.file_scoped_defs.is_empty());
        assert!(ex.meta_use.is_some(), "_use consumed as meta");
        assert!(ex.meta_ymx.is_none());
        assert!(ex.meta_test.is_none());
        assert!(ex.rejections.is_empty());
        let mv = ex.meta_use.take().unwrap();
        assert_eq!(mv.file, FileId(0));
    }

    #[test]
    fn duplicate_bare_use_in_same_document_first_wins() {
        let mut ex = extract(
            0,
            "_use:\n  x: a.b\n_use:\n  y: c.d\nmain: 1\n",
        );
        assert!(ex.meta_use.is_some());
        let mv = ex.meta_use.take().unwrap();
        assert_eq!(mv.file, FileId(0));
        assert!(ex.rejections.is_empty());
        assert_eq!(ex.defs.len(), 1, "main registers normally");
    }

    #[test]
    fn use_with_all_meta_in_same_file() {
        let ex = extract(
            0,
            "_ymx:\n  pretty: true\n_use:\n  \"*\": foo\n_test:\n  main: 1\nmain: 5\n",
        );
        assert_eq!(ex.defs.len(), 1, "main still registers normally");
        assert!(ex.meta_ymx.is_some());
        assert!(ex.meta_use.is_some());
        assert!(ex.meta_test.is_some());
        assert!(ex.rejections.is_empty());
    }

    #[test]
    fn dollar_use_variant_is_e015_not_consumed() {
        let ex = extract(0, "$_use: 1\nmain: 2\n");
        assert_eq!(ex.defs.len(), 1, "main registers normally");
        assert!(ex.meta_use.is_none(), "$_use is NOT consumed as meta");
        assert_eq!(ex.rejections.len(), 1);
        let diag = ex
            .rejections
            .into_iter()
            .next()
            .unwrap()
            .into_diagnostic(PathBuf::from("f.yml"))
            .unwrap();
        assert_eq!(diag.code, E015);
        assert_eq!(diag.component.as_deref(), Some("$_use"));
    }
}
