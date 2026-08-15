//! Entry-path resolution (invariant #1) and namespace-qualified lookup — and,
//! from milestone 1.6, the rule-1–16 resolver.
//!
//! The **entry path** is a file-path address `<folder.path>.<file>.<component>`
//! (e.g. `main.main` = root folder + `main.yml` + component `main`; `a.b.c` =
//! folder `a` + `b.yml` + component `c`). It is **not** a namespace dotted
//! path: `from: subdir.comp` and math `subdir.comp(...)` address namespaces,
//! while the entry pinpoints one file (the front-matter source) plus one
//! component for compilation.
//!
//! [`resolve_ref`] is the namespace lookup primitive used by `from`, bare
//! `$name` fallback, and builtins (milestone 1.6). Both functions are pure —
//! no I/O — because everything they need already lives in [`Project`] (root,
//! files, stores).
//!
//! `E009` (options stage) covers: malformed entry paths (fewer than two
//! segments, empty segments, separator-bearing segments), a missing entry
//! file, an ambiguous `.yml`/`.yaml` stem, and a component not defined in the
//! entry file — including names that can never be components (builtins, meta
//! keys, invalid identifiers). `resolve_ref` returns an explicit miss /
//! file-scope-violation outcome instead of a code: the call site (1.6) maps
//! [`LookupMiss::NotFound`] to `E002` and [`LookupMiss::FileScopeViolation`]
//! to `E005`.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::diag::{Diagnostic, FileId, Span, E002, E009};
use crate::interp;
use crate::ir::{Args, Value};
use crate::math::{Scope, V1Engine};
use crate::namespace::{classify, DefClass, Definition};
use crate::parse::{key_to_string, Node};
use crate::project::{Options, PlainMode, Project};

/// Resolve the entry path `<folder.path>.<file>.<component>` against an
/// already-loaded [`Project`].
///
/// Returns `(front-matter FileId, namespace, component)`:
/// * the [`FileId`] of the entry document (the front-matter source — its raw
///   `_ymx`/`_test` meta is what `ymx-config` / the CLI consume);
/// * the namespace the component lives in — the dotted folder path of the
///   entry (empty string for root-level files);
/// * the component name as written in the entry path.
///
/// Segment grammar: the penultimate segment is the file stem (extensionless);
/// all segments before it form the folder path (dotted in the entry, joined
/// with `/` on disk); the last segment is the component name. A component is
/// considered "defined in the entry file" if it is a non-`_` definition whose
/// hosting file is the entry document, or a file-scoped `_`-prefixed
/// definition stored for that document (file-scope restricts *references*, not
/// entry pinning — `--entry main._x` compiling `main.yml`'s `_x` is coherent).
///
/// `E009` failures: fewer than two segments; any empty or separator-bearing
/// segment; no `.<folder>/<stem>.yml` **and** no `.<folder>/<stem>.yaml`
/// (missing file — no `file` slot, the attempted path is in the message);
/// both extensions present (ambiguous stem); or the component not defined in
/// the entry file (`file` attached — the document exists). `E009` carries
/// `file: None` only when no loaded document is implicated (invariant #5).
pub fn resolve_entry<'a>(
    project: &Project,
    entry: &'a str,
) -> Result<(FileId, String, &'a str), Diagnostic> {
    let segments: Vec<&str> = entry.split('.').collect();
    if segments.len() < 2 {
        return Err(malformed(
            entry,
            "expected at least two segments (`<folder.path>.<file>.<component>`)",
        ));
    }
    for segment in &segments {
        if segment.is_empty() || segment.contains('/') || segment.contains('\\') {
            return Err(malformed(
                entry,
                "segments must be non-empty dotted-path parts (`<folder.path>.<file>.<component>`)",
            ));
        }
    }
    let folder = &segments[..segments.len() - 2];
    let stem = segments[segments.len() - 2];
    let component = segments[segments.len() - 1];
    let namespace = folder.join(".");

    let mut rel_dir = PathBuf::new();
    for segment in folder {
        rel_dir.push(segment);
    }

    let mut candidates: Vec<FileId> = Vec::new();
    for (idx, path) in project.files.iter().enumerate() {
        let Ok(relative) = path.strip_prefix(&project.root) else {
            continue;
        };
        for ext in ["yml", "yaml"] {
            if relative == rel_dir.join(stem).with_extension(ext) {
                candidates.push(FileId(idx as u32));
            }
        }
    }
    let file_id = match candidates.len() {
        0 => {
            return Err(Diagnostic {
                file: None,
                line: 1,
                col: 1,
                component: None,
                code: E009,
                message: format!(
                    "entry file not found: no `{}` under `{}` (entry `{entry}`)",
                    rel_dir.join(stem).with_extension("yml").display(),
                    project.root.display(),
                ),
            });
        }
        1 => candidates[0],
        2 => {
            return Err(Diagnostic {
                file: None,
                line: 1,
                col: 1,
                component: None,
                code: E009,
                message: format!(
                    "ambiguous entry `{entry}`: both `{}` and `{}` exist",
                    rel_dir.join(stem).with_extension("yml").display(),
                    rel_dir.join(stem).with_extension("yaml").display(),
                ),
            });
        }
        _ => unreachable!("at most one file per extension per stem"),
    };

    let file_path = project.files[file_id.0 as usize].clone();
    match classify(component, Span { line: 1, col: 1 }) {
        DefClass::Component(meta) if meta.file_scoped => {
            if project.file_scoped.get(file_id, component).is_some() {
                Ok((file_id, namespace, component))
            } else {
                Err(component_missing(entry, component, &file_path))
            }
        }
        DefClass::Component(_) => {
            let defined_in_entry_file = project
                .namespaces
                .get(&namespace, component)
                .map(|def| def.file == file_id)
                .unwrap_or(false);
            if defined_in_entry_file {
                Ok((file_id, namespace, component))
            } else {
                Err(component_missing(entry, component, &file_path))
            }
        }
        _ => Err(Diagnostic {
            file: Some(file_path),
            line: 1,
            col: 1,
            component: Some(component.to_string()),
            code: E009,
            message: format!(
                "`{component}` cannot be an entry component: it is not a component or template name (entry `{entry}`)"
            ),
        }),
    }
}

/// A malformed entry path: no document implicated, so no `file` slot.
fn malformed(entry: &str, detail: &str) -> Diagnostic {
    Diagnostic {
        file: None,
        line: 1,
        col: 1,
        component: None,
        code: E009,
        message: format!("invalid entry path `{entry}`: {detail}"),
    }
}

/// The entry document exists but does not define the component.
fn component_missing(entry: &str, component: &str, file_path: &Path) -> Diagnostic {
    Diagnostic {
        file: Some(file_path.to_path_buf()),
        line: 1,
        col: 1,
        component: Some(component.to_string()),
        code: E009,
        message: format!(
            "component `{component}` is not defined in `{}` (entry `{entry}`)",
            file_path.display()
        ),
    }
}

/// Why [`resolve_ref`] did not resolve a name. Callers (milestone 1.6) map
/// [`NotFound`](LookupMiss::NotFound) to `E002` (unknown component reference)
/// and [`FileScopeViolation`](LookupMiss::FileScopeViolation) to `E005`
/// (file-scope violation) — the miss/violation distinction is the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupMiss {
    /// No definition anywhere for this name.
    NotFound,
    /// A file-scoped `_`-prefixed name exists, but only in document(s) other
    /// than the referencing one. `owner` is the lowest-[`FileId`] document
    /// that defines it (deterministic for diagnostics).
    FileScopeViolation { owner: FileId },
}

/// Resolve a namespace-qualified reference (used by `from`, bare `$name`
/// fallback, and builtins in milestone 1.6) against an already-loaded
/// [`Project`].
///
/// `name` is the reference as written: a bare name (`main`, `$box`, `_x`) or a
/// dotted namespace address (`subdir.comp`, `subdir.$tbox`). `from_file` is
/// the referencing document's [`FileId`] — it decides file-scope visibility.
/// `plain` is the effective `_ymx.plain` mode (wired from [`Options`] by
/// `ymx-config` in milestone 1.4); `PlainMode::False` disables promotion.
///
/// Resolution order:
/// 1. **Dotted names** (`a.b`, `subdir.$tbox`) — the part before the last dot
///    is the namespace path, the rest (with any leading `$`s) is the name.
///    Namespaces never hold `_`-prefixed definitions (they are file-scoped),
///    so a dotted ref to a `_`-name is always [`LookupMiss::NotFound`].
/// 2. **File-scoped names** — the effective identifier (leading `$`s
///    stripped) starts with `_`. Looked up in the *referencing* document's
///    file-scope store by full name (`_x`, `$_a`, …); found → resolved. Not
///    found in `from_file` but present in another document →
///    [`LookupMiss::FileScopeViolation`]; absent everywhere →
///    [`LookupMiss::NotFound`].
/// 3. **Bare names** — global namespace first; on a miss, `plain` promotion
///    scans sub-namespaces in lexicographic dotted-path order (deterministic)
///    for the full name, promoting components **and** templates under
///    `PlainMode::All` but only templates (leading `$`) under
///    `PlainMode::TemplatesOnly`.
///
/// Names that can never be definitions — meta keys (`_ymx`, `_test`), builtin
/// effective ids (`map`/`reduce`/`merge`), reserved `$`-meta variants, invalid
/// identifiers — resolve to [`LookupMiss::NotFound`] defensively.
pub fn resolve_ref<'a>(
    project: &'a Project,
    name: &str,
    from_file: FileId,
    plain: PlainMode,
) -> Result<&'a Definition, LookupMiss> {
    if name.contains('.') {
        let Some(dot) = name.rfind('.') else {
            return Err(LookupMiss::NotFound);
        };
        let (namespace, short) = (&name[..dot], &name[dot + 1..]);
        return project
            .namespaces
            .get(namespace, short)
            .ok_or(LookupMiss::NotFound);
    }
    match classify(name, Span { line: 1, col: 1 }) {
        DefClass::Component(meta) if meta.file_scoped => {
            if let Some(def) = project.file_scoped.get(from_file, name) {
                return Ok(def);
            }
            let owner = project
                .file_scoped
                .defs()
                .filter(|(owner, full, _)| *owner != from_file && *full == name)
                .map(|(owner, _, _)| owner)
                .min_by_key(|owner| owner.0);
            match owner {
                Some(owner) => Err(LookupMiss::FileScopeViolation { owner }),
                None => Err(LookupMiss::NotFound),
            }
        }
        DefClass::Component(_) => {
            if let Some(def) = project.namespaces.get("", name) {
                return Ok(def);
            }
            if plain != PlainMode::False {
                let mut paths: Vec<&str> = project
                    .namespaces
                    .namespaces()
                    .map(|(path, _)| path)
                    .filter(|path| !path.is_empty())
                    .collect();
                paths.sort_unstable();
                let templates_only = plain == PlainMode::TemplatesOnly;
                for path in paths {
                    if let Some(def) = project.namespaces.get(path, name) {
                        if !templates_only || def.full_name.starts_with('$') {
                            return Ok(def);
                        }
                    }
                }
            }
            Err(LookupMiss::NotFound)
        }
        _ => Err(LookupMiss::NotFound),
    }
}

// ---- Rule-1–16 resolver (milestone 1.6) ----

/// Compile the namespace-qualified component `component` (a bare name resolved
/// in the global namespace or `plain`-promoted, or a dotted namespace path
/// `subdir.comp`) called with `args`, under `opts`.
///
/// For a bare `_`-prefixed (file-scoped) name there is no referencing
/// document, so the owning file is resolved as the lowest [`FileId`] that
/// defines the name (deterministic). `compile` resolves the entry path to the
/// definition directly and never relies on that search.
///
/// Errors carry the definition's host-file path, the offending span, and the
/// component name where sensible (invariant #5).
pub fn compile_component(
    project: &Project,
    component: &str,
    args: &Args,
    opts: &Options,
) -> Result<Value, Vec<Diagnostic>> {
    let def = locate_definition(project, component, opts.plain.clone()).map_err(|d| vec![d])?;
    Resolver::new(project, opts)
        .call(def, args)
        .map_err(|d| vec![d])
}

/// Convenience: resolve the entry path `opts.entry` (file-path form
/// `<folder.path>.<file>.<component>`, invariant #1) to the component defined
/// in the entry document and compile it with no args.
pub fn compile(project: &Project, opts: &Options) -> Result<Value, Vec<Diagnostic>> {
    let (file_id, namespace, component) =
        resolve_entry(project, &opts.entry).map_err(|d| vec![d])?;
    let def = if component.starts_with('_') {
        project.file_scoped.get(file_id, component)
    } else {
        project.namespaces.get(&namespace, component)
    };
    let Some(def) = def else {
        return Err(vec![Diagnostic {
            file: Some(project.files[file_id.0 as usize].clone()),
            line: 1,
            col: 1,
            component: Some(component.to_string()),
            code: E002,
            message: format!("component `{component}` is not defined in the entry file"),
        }]);
    };
    Resolver::new(project, opts)
        .call(def, &Args::None)
        .map_err(|d| vec![d])
}

/// Locate the definition a `compile_component` call names: a bare non-`_` name
/// resolves via [`resolve_ref`] (global namespace first, then `plain`
/// promotion); a dotted name resolves against its namespace; a bare `_`
/// (file-scoped) name resolves to its owner (lowest [`FileId`], deterministic).
/// Misses are `E002`.
fn locate_definition<'a>(
    project: &'a Project,
    component: &str,
    plain: PlainMode,
) -> Result<&'a Definition, Diagnostic> {
    if component.starts_with('_') && !component.contains('.') {
        let mut owners: Vec<(u32, &Definition)> = project
            .file_scoped
            .defs()
            .filter(|(_, full, _)| *full == component)
            .map(|(fid, _, def)| (fid.0, def))
            .collect();
        owners.sort_unstable_by_key(|(id, _)| *id);
        match owners.into_iter().next() {
            Some((_, def)) => Ok(def),
            None => Err(unknown_component(component)),
        }
    } else {
        match resolve_ref(project, component, FileId(0), plain) {
            Ok(def) => Ok(def),
            // FileScopeViolation is unreachable here: the `_`-prefixed branch
            // above owns all file-scoped names.
            Err(_) => Err(unknown_component(component)),
        }
    }
}

fn unknown_component(component: &str) -> Diagnostic {
    Diagnostic {
        file: None,
        line: 1,
        col: 1,
        component: Some(component.to_string()),
        code: E002,
        message: format!("unknown component reference `{component}`"),
    }
}

/// The rule-1–16 resolver: compiles one component at a time against an
/// already-loaded [`Project`]. Created per top-level `compile` /
/// `compile_component` call so the recursion state (depth cap, milestone 1.6
/// task 9) is per-compilation.
struct Resolver<'a> {
    project: &'a Project,
}

impl<'a> Resolver<'a> {
    fn new(project: &'a Project, _opts: &'a Options) -> Resolver<'a> {
        Resolver { project }
    }

    /// Resolve `def` as a normal component call with `args`. Milestone 1.6
    /// task 1: the rule-11 pipeline is a single body-resolution step; the
    /// template chain (task 5) and `from`/shortcut dispatch (tasks 6–7) slot
    /// in around it.
    fn call(&self, def: &Definition, args: &Args) -> Result<Value, Diagnostic> {
        let scope = self.scope_for(def, args);
        self.resolve_body(&def.body, &scope)
    }

    /// The evaluation scope for `def` called with `args`: named/positional
    /// arguments bound per rules 2/4, the definition's host-file path and key
    /// span as diagnostic context.
    fn scope_for(&self, def: &Definition, args: &Args) -> Scope {
        Scope {
            file: Some(self.project.files[def.file.0 as usize].clone()),
            component: Some(def.full_name.clone()),
            span: def.span,
            named: args.named_vec(),
            positional: args.positional_vec(),
            last: None,
            call: None,
        }
    }

    /// Step 1 of rule 11 — property resolution. Task 1: the body resolves as a
    /// plain value (scalars, arrays, objects, interpolated strings); nested
    /// call-sites, mini-components, and key handling land with tasks 2–4.
    fn resolve_body(&self, node: &Node, scope: &Scope) -> Result<Value, Diagnostic> {
        self.resolve_node(node, scope)
    }

    /// Resolve one value node against `scope`. String scalars go through the
    /// shared scanner/interpolator (bare `$name` / `$N` / `${...}`); a missing
    /// argument is `E003` (rule 10) until the component fallback lands in
    /// milestone 1.6 task 2.
    fn resolve_node(&self, node: &Node, scope: &Scope) -> Result<Value, Diagnostic> {
        match node {
            Node::Null(_) => Ok(Value::Null),
            Node::Bool(b, _) => Ok(Value::Bool(*b)),
            Node::Int(i, _) => Ok(Value::Int(*i)),
            Node::Float(f, _) => Ok(Value::Float(*f)),
            Node::String(s, span) => {
                let segments = interp::scan(s, *span)?;
                interp::resolve(&segments, scope, &V1Engine)
            }
            Node::Array(items, _) => items
                .iter()
                .map(|n| self.resolve_node(n, scope))
                .collect::<Result<Vec<Value>, _>>()
                .map(Value::array),
            Node::Object(entries, _) => {
                let mut m = IndexMap::with_capacity(entries.len());
                for entry in entries {
                    m.insert(
                        key_to_string(&entry.key),
                        self.resolve_node(&entry.value, scope)?,
                    );
                }
                Ok(Value::Object(m))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::namespace::Definition;
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
    /// * `main.yml`      (FileId 0): `main`, `$box`; file-scoped `_x`, `$_a`
    /// * `a/b.yml`       (FileId 1): `x`, `c`; file-scoped `_x`
    /// * `a/other.yml`   (FileId 2): `y`
    /// * `subdir/t.yml`  (FileId 3): `t`, `$tbox`, `x`
    fn project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![
            PathBuf::from("/proj/main.yml"),
            PathBuf::from("/proj/a/b.yml"),
            PathBuf::from("/proj/a/other.yml"),
            PathBuf::from("/proj/subdir/t.yml"),
        ];
        p.namespaces.register("", def(0, "main")).unwrap();
        p.namespaces.register("", def(0, "$box")).unwrap();
        p.namespaces.register("a", def(1, "x")).unwrap();
        p.namespaces.register("a", def(1, "c")).unwrap();
        p.namespaces.register("a", def(2, "y")).unwrap();
        p.namespaces.register("subdir", def(3, "t")).unwrap();
        p.namespaces.register("subdir", def(3, "$tbox")).unwrap();
        p.namespaces.register("subdir", def(3, "x")).unwrap();
        p.file_scoped.register(FileId(0), def(0, "_x")).unwrap();
        p.file_scoped.register(FileId(0), def(0, "$_a")).unwrap();
        p.file_scoped.register(FileId(1), def(1, "_x")).unwrap();
        p
    }

    #[test]
    fn entry_resolves_folder_file_component() {
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "a.b.c").unwrap();
        assert_eq!(file_id, FileId(1));
        assert_eq!(namespace, "a");
        assert_eq!(component, "c");
    }

    #[test]
    fn default_entry_main_main_resolves_root_file() {
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "main.main").unwrap();
        assert_eq!(file_id, FileId(0));
        assert_eq!(namespace, "");
        assert_eq!(component, "main");
    }

    #[test]
    fn template_component_is_reachable_via_entry() {
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "main.$box").unwrap();
        assert_eq!(file_id, FileId(0));
        assert_eq!(namespace, "");
        assert_eq!(component, "$box");
    }

    #[test]
    fn file_scoped_component_is_reachable_via_entry() {
        // File-scope restricts cross-document *references* (E005, 1.6), not
        // entry pinning: `--entry main._x` compiles main.yml's `_x`.
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "main._x").unwrap();
        assert_eq!(file_id, FileId(0));
        assert_eq!(namespace, "");
        assert_eq!(component, "_x");
    }

    #[test]
    fn one_segment_is_e009() {
        let p = project();
        let err = resolve_entry(&p, "main").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(err.file, None, "no document implicated by a malformed path");
        assert!(err.message.contains("main"));
    }

    #[test]
    fn empty_and_separator_segments_are_e009() {
        let p = project();
        for entry in ["a..c", ".a.b", "a.b.", "a/b.c", "a\\b.c"] {
            let err = resolve_entry(&p, entry).unwrap_err();
            assert_eq!(err.code, E009, "{entry}");
            assert_eq!(err.file, None, "{entry}");
        }
    }

    #[test]
    fn missing_file_is_e009_with_attempted_path() {
        let p = project();
        let err = resolve_entry(&p, "a.missing.c").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(err.file, None, "the file is not loaded; no FileId exists");
        assert!(
            err.message.contains("a/missing.yml"),
            "message renders the attempted path: {}",
            err.message
        );
    }

    #[test]
    fn ambiguous_stem_is_e009() {
        let mut p = project();
        p.files.push(PathBuf::from("/proj/a/b.yaml"));
        let err = resolve_entry(&p, "a.b.c").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(err.file, None);
        assert!(err.message.contains("b.yml"), "{}", err.message);
        assert!(err.message.contains("b.yaml"), "{}", err.message);
    }

    #[test]
    fn component_not_defined_in_entry_file_is_e009() {
        let p = project();
        // `y` exists in namespace `a` but is defined by a/other.yml, not a/b.yml.
        let err = resolve_entry(&p, "a.b.y").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(
            err.file.as_deref(),
            Some(Path::new("/proj/a/b.yml")),
            "the entry document exists, so it is attached"
        );
        assert_eq!(err.component.as_deref(), Some("y"));
        assert!(err.message.contains("a/b.yml"), "{}", err.message);

        // The same name via its actual file resolves fine.
        let (file_id, namespace, component) = resolve_entry(&p, "a.other.y").unwrap();
        assert_eq!(file_id, FileId(2));
        assert_eq!(namespace, "a");
        assert_eq!(component, "y");
    }

    #[test]
    fn meta_and_builtin_names_cannot_be_entry_components() {
        let p = project();
        for entry in ["main._ymx", "main._test", "main.map", "main.1x"] {
            let err = resolve_entry(&p, entry).unwrap_err();
            assert_eq!(err.code, E009, "{entry}");
            assert_eq!(
                err.file.as_deref(),
                Some(Path::new("/proj/main.yml")),
                "{entry}: the document exists"
            );
        }
    }

    // ---- Task 6: namespace-qualified lookup ----

    fn lookup<'a>(
        project: &'a Project,
        name: &str,
        from_file: u32,
    ) -> Result<&'a Definition, LookupMiss> {
        resolve_ref(project, name, FileId(from_file), PlainMode::False)
    }

    #[test]
    fn bare_name_hits_global_namespace() {
        let p = project();
        let main = lookup(&p, "main", 0).expect("global main");
        assert_eq!(main.file, FileId(0));
        assert_eq!(main.full_name, "main");
        let boxed = lookup(&p, "$box", 0).expect("global template $box");
        assert_eq!(boxed.full_name, "$box");
        // Global definitions are visible from every document.
        let from_other = lookup(&p, "main", 2).expect("global visible cross-document");
        assert_eq!(from_other.file, FileId(0));
    }

    #[test]
    fn dotted_ref_hits_subnamespace() {
        let p = project();
        let x = lookup(&p, "a.x", 0).expect("a.x");
        assert_eq!(x.file, FileId(1));
        let t = lookup(&p, "subdir.t", 0).expect("subdir.t");
        assert_eq!(t.file, FileId(3));
        let tbox = lookup(&p, "subdir.$tbox", 0).expect("subdir.$tbox template");
        assert_eq!(tbox.full_name, "$tbox");
    }

    #[test]
    fn dotted_ref_miss_is_not_found() {
        let p = project();
        assert_eq!(lookup(&p, "a.nope", 0).err(), Some(LookupMiss::NotFound));
        assert_eq!(
            lookup(&p, "subdir.inner.x", 0).err(),
            Some(LookupMiss::NotFound)
        );
        assert_eq!(lookup(&p, "a.b", 0).err(), Some(LookupMiss::NotFound));
        assert_eq!(lookup(&p, "a.", 0).err(), Some(LookupMiss::NotFound));
    }

    #[test]
    fn dotted_ref_to_file_scoped_name_is_not_found() {
        // `_`-prefixed definitions never enter a namespace: `subdir._x` is
        // absent from the `subdir` namespace even though a doc under subdir/
        // might own a file-scoped `_x` (call sites map this to E002).
        let p = project();
        assert_eq!(lookup(&p, "subdir._x", 3).err(), Some(LookupMiss::NotFound));
        assert_eq!(lookup(&p, "a._x", 1).err(), Some(LookupMiss::NotFound));
    }

    #[test]
    fn file_scoped_hit_from_owning_document() {
        let p = project();
        let x = lookup(&p, "_x", 0).expect("owning doc resolves its _x");
        assert_eq!(x.file, FileId(0));
        let x = lookup(&p, "_x", 1).expect("a/b.yml resolves its own _x");
        assert_eq!(x.file, FileId(1));
        let a = lookup(&p, "$_a", 0).expect("owning doc resolves file-scoped template");
        assert_eq!(a.full_name, "$_a");
    }

    #[test]
    fn file_scoped_ref_from_other_document_is_violation() {
        let p = project();
        let err = lookup(&p, "_x", 2).expect_err("a/other.yml does not own _x");
        assert_eq!(
            err,
            LookupMiss::FileScopeViolation { owner: FileId(0) },
            "lowest owning FileId reported (deterministic)"
        );
        let err = lookup(&p, "$_a", 2).expect_err("file-scoped template violates too");
        assert_eq!(err, LookupMiss::FileScopeViolation { owner: FileId(0) });
    }

    #[test]
    fn file_scoped_name_absent_anywhere_is_not_found() {
        let p = project();
        assert_eq!(lookup(&p, "_z", 0).err(), Some(LookupMiss::NotFound));
        assert_eq!(lookup(&p, "_z", 2).err(), Some(LookupMiss::NotFound));
    }

    #[test]
    fn meta_and_builtin_names_never_resolve() {
        let p = project();
        for name in [
            "_ymx", "_test", "map", "reduce", "merge", "$_ymx", "$$_test",
        ] {
            assert_eq!(
                lookup(&p, name, 0).err(),
                Some(LookupMiss::NotFound),
                "{name}"
            );
        }
    }

    #[test]
    fn promotion_all_promotes_components_and_templates() {
        let p = project();
        let promoted = resolve_ref(&p, "x", FileId(0), PlainMode::All).expect("component promoted");
        assert_eq!(
            promoted.file,
            FileId(1),
            "`a` sorts before `subdir` — lexicographic scan"
        );
        let t = resolve_ref(&p, "t", FileId(0), PlainMode::All).expect("component promoted");
        assert_eq!(t.file, FileId(3));
        let tbox = resolve_ref(&p, "$tbox", FileId(0), PlainMode::All).expect("template promoted");
        assert_eq!(tbox.full_name, "$tbox");
    }

    #[test]
    fn promotion_templates_only_promotes_templates_but_not_components() {
        let p = project();
        let tbox = resolve_ref(&p, "$tbox", FileId(0), PlainMode::TemplatesOnly)
            .expect("template promoted");
        assert_eq!(tbox.full_name, "$tbox");
        assert_eq!(
            resolve_ref(&p, "x", FileId(0), PlainMode::TemplatesOnly).err(),
            Some(LookupMiss::NotFound),
            "components are not promoted under TemplatesOnly"
        );
        assert_eq!(
            resolve_ref(&p, "t", FileId(0), PlainMode::TemplatesOnly).err(),
            Some(LookupMiss::NotFound)
        );
    }

    #[test]
    fn promotion_disabled_by_default_mode() {
        let p = project();
        assert_eq!(lookup(&p, "x", 0).err(), Some(LookupMiss::NotFound));
        assert_eq!(lookup(&p, "t", 0).err(), Some(LookupMiss::NotFound));
        assert_eq!(lookup(&p, "$tbox", 0).err(), Some(LookupMiss::NotFound));
    }

    #[test]
    fn promotion_never_shadows_global_or_owner() {
        let p = project();
        // A name already in the global namespace wins without scanning.
        let main = resolve_ref(&p, "main", FileId(2), PlainMode::All).expect("global wins");
        assert_eq!(main.file, FileId(0));
        // File-scoped resolution is not affected by promotion.
        let x = resolve_ref(&p, "_x", FileId(1), PlainMode::All).expect("owner wins");
        assert_eq!(x.file, FileId(1));
    }

    // ---- Milestone 1.6 task 1: compile / compile_component ----

    /// Build a [`Project`] from `(relative_path, yaml_source)` pairs (no I/O —
    /// ymx-core is I/O-free). The namespace of a definition is the directory
    /// of its file (`""` for root files, dotted for subdirectories), mirroring
    /// `load_project`.
    fn project_with(files: &[(&str, &str)]) -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        for (i, (path, src)) in files.iter().enumerate() {
            p.files.push(PathBuf::from("/proj").join(path));
            let node = crate::parse::parse_document(src).expect("parse fixture");
            let ex = crate::namespace::extract_document(FileId(i as u32), &node);
            let namespace = Path::new(path)
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .replace('/', ".");
            for def in ex.defs {
                p.namespaces.register(&namespace, def).unwrap();
            }
            for def in ex.file_scoped_defs {
                p.file_scoped.register(FileId(i as u32), def).unwrap();
            }
        }
        p
    }

    fn named(entries: &[(&str, Value)]) -> Args {
        Args::Named(
            entries
                .iter()
                .map(|(n, v)| (n.to_string(), v.clone()))
                .collect(),
        )
    }

    fn compile_ok(p: &Project, component: &str, args: &Args) -> Value {
        compile_component(p, component, args, &Options::default())
            .unwrap_or_else(|ds| panic!("{component}: {}", ds[0].message))
    }

    fn compile_err(p: &Project, component: &str, args: &Args) -> Diagnostic {
        compile_component(p, component, args, &Options::default())
            .unwrap_err()
            .into_iter()
            .next()
            .expect("at least one diagnostic")
    }

    #[test]
    fn compile_component_bare_global_and_dotted() {
        let p = project_with(&[
            ("main.yml", "main: hello\n"),
            ("subdir/t.yml", "comp: 5\nbox: 1.5\n"),
        ]);
        assert_eq!(compile_ok(&p, "main", &Args::None), Value::string("hello"));
        assert_eq!(compile_ok(&p, "subdir.comp", &Args::None), Value::int(5));
        assert_eq!(compile_ok(&p, "subdir.box", &Args::None), Value::float(1.5));
    }

    #[test]
    fn compile_component_unknown_name_is_e002() {
        let p = project_with(&[("main.yml", "main: 1\n")]);
        for component in ["nope", "subdir.nope", "a.b.c"] {
            let d = compile_err(&p, component, &Args::None);
            assert_eq!(d.code, E002, "{component}");
            assert_eq!(d.component.as_deref(), Some(component), "{component}");
        }
    }

    #[test]
    fn compile_component_plain_promotion_for_bare_names() {
        let p = project_with(&[("main.yml", "main: 1\n"), ("subdir/x.yml", "x: 7\n")]);
        let err = compile_err(&p, "x", &Args::None);
        assert_eq!(err.code, E002, "no promotion under the default plain mode");
        let opts = Options {
            plain: PlainMode::All,
            ..Options::default()
        };
        assert_eq!(
            compile_component(&p, "x", &Args::None, &opts).unwrap(),
            Value::int(7),
            "PlainMode::All promotes sub-namespace components"
        );
        // The dotted qualified path stays reachable alongside the promoted name.
        assert_eq!(compile_ok(&p, "subdir.x", &Args::None), Value::int(7));
    }

    #[test]
    fn compile_component_file_scoped_owner_search() {
        let p = project_with(&[
            ("main.yml", "_secret: 41\nmain: 1\n"),
            ("a/b.yml", "_secret: 42\nb: 2\n"),
        ]);
        assert_eq!(
            compile_ok(&p, "_secret", &Args::None),
            Value::int(41),
            "the lowest owning FileId wins deterministically"
        );
    }

    #[test]
    fn compile_component_binds_named_and_positional_args() {
        let p = project_with(&[(
            "main.yml",
            "user:\n  name: $user_name\n  phone: $user_phone\nmain: $0 + $1\n",
        )]);
        assert_eq!(
            compile_ok(
                &p,
                "user",
                &named(&[
                    ("user_name", Value::string("Mathew")),
                    ("user_phone", Value::int(123456789))
                ]),
            ),
            Value::object(IndexMap::from([
                ("name".to_string(), Value::string("Mathew")),
                ("phone".to_string(), Value::int(123456789)),
            ]))
        );
        assert_eq!(
            compile_ok(
                &p,
                "main",
                &Args::Positional(vec![Value::int(12), Value::int(34)])
            ),
            Value::string("12 + 34")
        );
        assert_eq!(
            compile_ok(
                &p,
                "main",
                &Args::Mixed {
                    named: vec![("x".to_string(), Value::int(1))],
                    positional: vec![Value::int(12), Value::int(34)],
                }
            ),
            Value::string("12 + 34")
        );
    }

    #[test]
    fn compile_component_resolves_plain_structures() {
        let p = project_with(&[(
            "main.yml",
            "main:\n  a: [1, 2.5, true, null, \"x\"]\n  b:\n    c: hi\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                (
                    "a".to_string(),
                    Value::array(vec![
                        Value::int(1),
                        Value::float(2.5),
                        Value::bool(true),
                        Value::null(),
                        Value::string("x"),
                    ])
                ),
                (
                    "b".to_string(),
                    Value::object(IndexMap::from([("c".to_string(), Value::string("hi"))]))
                ),
            ]))
        );
    }

    #[test]
    fn compile_resolves_entry_path_to_qualified_component() {
        let p = project_with(&[
            ("main.yml", "main: 1\n_private: 2\n"),
            ("a/b.yml", "c: 3\n"),
            ("subdir/t.yml", "t: 4\n"),
        ]);
        let opts = Options::default();
        assert_eq!(compile(&p, &opts).unwrap(), Value::int(1));
        let opts = Options {
            entry: "a.b.c".to_string(),
            ..Options::default()
        };
        assert_eq!(compile(&p, &opts).unwrap(), Value::int(3));
        let opts = Options {
            entry: "main._private".to_string(),
            ..Options::default()
        };
        assert_eq!(compile(&p, &opts).unwrap(), Value::int(2));
    }

    #[test]
    fn compile_missing_entry_component_is_e009() {
        let p = project_with(&[("main.yml", "other: 1\n")]);
        let ds = compile(&p, &Options::default()).unwrap_err();
        assert_eq!(ds[0].code, E009);
        assert_eq!(ds[0].file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert_eq!(ds[0].component.as_deref(), Some("main"));
    }
}
