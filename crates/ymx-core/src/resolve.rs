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
use std::rc::Rc;

use indexmap::IndexMap;

use crate::callsite;
use crate::diag::{Diagnostic, FileId, Span, E002, E005, E009, E010};
use crate::interp;
use crate::ir::{Args, Value};
use crate::math::{FallbackHook, Scope, V1Engine};
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
    opts: &'a Options,
}

impl<'a> Resolver<'a> {
    fn new(project: &'a Project, opts: &'a Options) -> Resolver<'a> {
        Resolver { project, opts }
    }

    /// Resolve `def` as a normal component call with `args`. Milestone 1.6
    /// task 3: the rule-11 pipeline is step 1 (property resolution incl. the
    /// rule-4 slots, the rule-2 fallback, and rule-3 inline call-sites)
    /// followed by the output conversion; the template chain (task 5) and
    /// `from`/shortcut dispatch (tasks 6–7) slot in around it.
    fn call(&self, def: &Definition, args: &Args) -> Result<Value, Diagnostic> {
        let scope = self.scope_for(def, args);
        let body = self.resolve_body(&def.body, &scope, def.file)?;
        Ok(match body {
            ResolvedBody::Value(v) => v,
            ResolvedBody::Object(set) => set.to_object(),
        })
    }

    /// The evaluation scope for `def` called with `args`: named/positional
    /// arguments bound per rules 2/4, the definition's host-file path and key
    /// span as diagnostic context, and the rule-2 bare-`$name` fallback hook
    /// (looks up the regular component `name` from `def`'s file and calls it
    /// with no args; `_`-prefixed names resolve file-scoped).
    fn scope_for<'s>(&'s self, def: &Definition, args: &Args) -> Scope<'s> {
        let file = def.file;
        let fallback: FallbackHook<'s> =
            Rc::new(move |name: &str| self.lookup_component(file, name));
        Scope {
            file: Some(self.project.files[def.file.0 as usize].clone()),
            component: Some(def.full_name.clone()),
            span: def.span,
            named: args.named_vec(),
            positional: args.positional_vec(),
            last: None,
            call: None,
            fallback: Some(fallback),
        }
    }

    /// Rule-2 fallback (b): a regular component `name` reachable from `file`
    /// is called with no args. `NotFound` / file-scope violations yield
    /// `Ok(None)` so the caller reports the plain `E003`.
    fn lookup_component(&self, file: FileId, name: &str) -> Result<Option<Value>, Diagnostic> {
        match resolve_ref(self.project, name, file, self.opts.plain.clone()) {
            Ok(def) => Ok(Some(self.call(def, &Args::None)?)),
            Err(LookupMiss::NotFound | LookupMiss::FileScopeViolation { .. }) => Ok(None),
        }
    }

    /// Step 1 of rule 11 — property resolution. The component body resolves
    /// as either a plain value or a property set: an object body is a
    /// [`PropertySet`] where non-negative integer keys denote positional
    /// slots (rule 4); every other node resolves as a plain value (arrays,
    /// scalars, interpolated strings, nested objects). `file` is the
    /// referencing document for name lookups (call-sites, fallback).
    fn resolve_body(
        &self,
        node: &Node,
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<ResolvedBody, Diagnostic> {
        match node {
            Node::Object(entries, _) => self.resolve_property_set(entries, scope, file),
            other => self
                .resolve_node(other, scope, file)
                .map(ResolvedBody::Value),
        }
    }

    /// Resolve an object component body into a [`PropertySet`] (rule 4):
    /// integer keys `0..N` are slots — defaults for `$0..$N` that the call's
    /// positional arguments overwrite; string keys (and negative/non-integer
    /// keys) are ordinary named properties, the string `"0"` included.
    ///
    /// Two phases: the slots resolve first against the call's positional
    /// arguments (a slot default referencing `$N` sees only the call's
    /// positional args), then the named properties resolve against the
    /// positional arguments padded with the slot defaults for every index the
    /// call did not provide (rule 4: a body may provide a default `$N` by
    /// writing the integer key).
    fn resolve_property_set(
        &self,
        entries: &[crate::parse::Entry],
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<ResolvedBody, Diagnostic> {
        let mut set = PropertySet::default();
        for entry in entries {
            match &entry.key {
                crate::parse::Key::Int(i) if *i >= 0 => {
                    let idx = *i as usize;
                    if idx > MAX_SLOTS {
                        return Err(ctx_err(
                            scope,
                            E010,
                            format!("slot key `{i}` is too large (max {MAX_SLOTS})"),
                        ));
                    }
                    // The call's positional argument overwrites the slot
                    // (rule 4: a call may set a positional slot via the
                    // integer key); the body value is the default.
                    let default = self.resolve_node(&entry.value, scope, file)?;
                    let value = scope.positional.get(idx).cloned().unwrap_or(default);
                    if set.slots.len() <= idx {
                        set.slots.resize(idx + 1, Value::Null);
                    }
                    set.slots[idx] = value;
                    set.order.push(PropKey::Slot(idx));
                }
                _ => {
                    let name = key_to_string(&entry.key);
                    set.order.push(PropKey::Named(name));
                }
            }
        }
        let padded = Scope {
            positional: padded_positional(&scope.positional, &set.slots),
            ..scope.clone()
        };
        for entry in entries {
            match &entry.key {
                crate::parse::Key::String(name) => {
                    let value = self.resolve_node(&entry.value, &padded, file)?;
                    set.named.insert(name.clone(), value);
                }
                crate::parse::Key::Int(i) if *i < 0 => {
                    let name = i.to_string();
                    let value = self.resolve_node(&entry.value, &padded, file)?;
                    set.named.insert(name, value);
                }
                _ => {}
            }
        }
        Ok(ResolvedBody::Object(set))
    }

    /// Resolve one value node against `scope`. String scalars go through the
    /// shared scanner/interpolator: `$name` / `$N` / `${...}`, with the rule-2
    /// component fallback after a named-argument miss — unless the whole
    /// string is an inline call-site `$name(...)` (rule 3), which calls the
    /// component directly. `file` is the referencing document for name
    /// lookups.
    fn resolve_node(
        &self,
        node: &Node,
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<Value, Diagnostic> {
        match node {
            Node::Null(_) => Ok(Value::Null),
            Node::Bool(b, _) => Ok(Value::Bool(*b)),
            Node::Int(i, _) => Ok(Value::Int(*i)),
            Node::Float(f, _) => Ok(Value::Float(*f)),
            Node::String(s, span) => self.resolve_string(s, *span, scope, file),
            Node::Array(items, _) => items
                .iter()
                .map(|n| self.resolve_node(n, scope, file))
                .collect::<Result<Vec<Value>, _>>()
                .map(Value::array),
            Node::Object(entries, _) => {
                let mut m = IndexMap::with_capacity(entries.len());
                for entry in entries {
                    m.insert(
                        key_to_string(&entry.key),
                        self.resolve_node(&entry.value, scope, file)?,
                    );
                }
                Ok(Value::Object(m))
            }
        }
    }

    /// Resolve a string scalar: a whole-string `$name(...)` is an inline
    /// call-site (rule 3); anything else goes through interpolation.
    fn resolve_string(
        &self,
        s: &str,
        span: Span,
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<Value, Diagnostic> {
        match callsite::parse(s) {
            Ok(Some(call)) => self.resolve_call(&call, span, scope, file),
            Ok(None) => {
                let segments = interp::scan(s, span)?;
                interp::resolve(&segments, scope, &V1Engine)
            }
            Err((code, message)) => Err(ctx_err(scope, code, message)),
        }
    }

    /// Resolve a parsed inline call-site: evaluate its arguments against the
    /// caller's scope (nested call-sites recurse), then call the target
    /// component. `$name(...)` unconditionally calls the component and
    /// bypasses the argument lookup (rule 2).
    fn resolve_call(
        &self,
        call: &callsite::ParsedCall,
        span: Span,
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<Value, Diagnostic> {
        let (named, positional) = self.resolve_call_args(&call.args, span, scope, file)?;
        let args = match (named.is_empty(), positional.is_empty()) {
            (true, true) => Args::None,
            (false, true) => Args::Named(named),
            (true, false) => Args::Positional(positional),
            (false, false) => Args::Mixed { named, positional },
        };
        self.call_by_name(file, &call.name, &args, span)
    }

    /// Evaluate a call-site argument list against `scope` (the rule-2
    /// fallback and math apply inside argument values).
    fn resolve_call_args(
        &self,
        args: &[callsite::ParsedArg],
        span: Span,
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<CallArgs, Diagnostic> {
        let mut named = Vec::new();
        let mut positional = Vec::new();
        for arg in args {
            let value = self.resolve_parsed_value(&arg.value, span, scope, file)?;
            match &arg.key {
                Some(key) => named.push((key.clone(), value)),
                None => positional.push(value),
            }
        }
        Ok((named, positional))
    }

    /// Resolve one parsed argument value. `$name` / `$N` references and
    /// `${...}` expressions re-enter the scanner/interpolator (so the rule-2
    /// fallback and math apply); nested call-sites recurse.
    fn resolve_parsed_value(
        &self,
        value: &callsite::ParsedValue,
        span: Span,
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<Value, Diagnostic> {
        match value {
            callsite::ParsedValue::Literal(v) => Ok(v.clone()),
            callsite::ParsedValue::Raw(s) => Ok(Value::string(s.clone())),
            callsite::ParsedValue::Ref { name } => {
                let segments = interp::scan(&format!("${name}"), span)?;
                interp::resolve(&segments, scope, &V1Engine)
            }
            callsite::ParsedValue::Math { src } => {
                let segments = interp::scan(&format!("${{{src}}}"), span)?;
                interp::resolve(&segments, scope, &V1Engine)
            }
            callsite::ParsedValue::Call(nested) => self.resolve_call(nested, span, scope, file),
        }
    }

    /// Call the regular component `name` reachable from `file` with `args`.
    /// `NotFound` is `E002`; a file-scope violation (`_`-prefixed name owned
    /// by another document) is `E005`.
    fn call_by_name(
        &self,
        file: FileId,
        name: &str,
        args: &Args,
        span: Span,
    ) -> Result<Value, Diagnostic> {
        match resolve_ref(self.project, name, file, self.opts.plain.clone()) {
            Ok(def) => self.call(def, args),
            Err(LookupMiss::NotFound) => Err(Diagnostic {
                file: Some(self.project.files[file.0 as usize].clone()),
                line: span.line,
                col: span.col,
                component: Some(name.to_string()),
                code: E002,
                message: format!("unknown component reference `{name}`"),
            }),
            Err(LookupMiss::FileScopeViolation { owner }) => Err(Diagnostic {
                file: Some(self.project.files[file.0 as usize].clone()),
                line: span.line,
                col: span.col,
                component: Some(name.to_string()),
                code: E005,
                message: format!(
                    "file-scope violation: `{name}` is defined only in `{}`",
                    self.project.files[owner.0 as usize].display()
                ),
            }),
        }
    }
}

/// Upper bound on slot indices (a cap far beyond any real document; guard
/// against a hostile `999999999:` key resizing the slots vector).
const MAX_SLOTS: usize = 65_535;

/// A resolved object component body (rule 11 step 1): the named properties,
/// the positional slots, and the source order for output.
#[derive(Default)]
struct PropertySet {
    /// Named properties (string keys and stringified non-slot keys).
    named: IndexMap<String, Value>,
    /// Slot values (`$N` defaults, overwritten by the call's positional
    /// arguments).
    slots: Vec<Value>,
    /// Source order of the body's keys, for output and later chain views.
    order: Vec<PropKey>,
}

/// One key of a resolved property set, in source order.
#[derive(Debug, Clone, PartialEq)]
enum PropKey {
    /// A named property key.
    Named(String),
    /// A positional slot (integer key `0`, `1`, …).
    Slot(usize),
}

/// The result of rule-11 step 1: a plain value or an object property set.
enum ResolvedBody {
    Value(Value),
    Object(PropertySet),
}

/// The result of evaluating a call-site argument list: named + positional.
type CallArgs = (Vec<(String, Value)>, Vec<Value>);

impl PropertySet {
    /// The output object: keys in source order, slots stringified as their
    /// decimal index, duplicates dropped (first occurrence wins).
    fn to_object(&self) -> Value {
        let mut m = IndexMap::with_capacity(self.order.len());
        for key in &self.order {
            let (name, value) = match key {
                PropKey::Named(name) => (name.clone(), self.named[name].clone()),
                PropKey::Slot(idx) => (idx.to_string(), self.slots[*idx].clone()),
            };
            m.entry(name).or_insert(value);
        }
        Value::Object(m)
    }
}

/// The call's positional arguments padded with the slot defaults for every
/// index the call did not provide (rule 4: slots are defaults).
fn padded_positional(call: &[Value], slots: &[Value]) -> Vec<Value> {
    let mut v = call.to_vec();
    if v.len() < slots.len() {
        v.extend_from_slice(&slots[v.len()..]);
    }
    v
}

/// A diagnostic attributed to `scope`'s file/component context at its base
/// span.
fn ctx_err(scope: &Scope<'_>, code: &'static str, message: String) -> Diagnostic {
    Diagnostic {
        file: scope.file.clone(),
        line: scope.span.line,
        col: scope.span.col,
        component: scope.component.clone(),
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{Span, E003, E012, E013};
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

    // ---- Milestone 1.6 task 2: arg binding, rule-2 fallback, rule-4 slots ----

    #[test]
    fn bare_dollar_name_fallback_order() {
        let p = project_with(&[(
            "main.yml",
            "x: 5\ncaller: \"got $x\"\nmissing: \"$nope\"\nargs_user: \"hi $0\"\ngreeter: \"$args_user\"\n",
        )]);
        // (b) no named arg in scope -> regular component called with no args.
        assert_eq!(
            compile_ok(&p, "caller", &Args::None),
            Value::string("got 5")
        );
        // (a) named arg in scope wins over the component.
        assert_eq!(
            compile_ok(&p, "caller", &named(&[("x", Value::string("arg"))])),
            Value::string("got arg")
        );
        // (c) neither -> E003.
        let d = compile_err(&p, "missing", &Args::None);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("nope"), "{}", d.message);
        // The fallback call passes no args: `args_user` needs `$0`, so the
        // error surfaces from inside the callee (even when the caller itself
        // was invoked with positional args — the fallback is always no-args).
        let d = compile_err(&p, "greeter", &Args::None);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("$0"), "{}", d.message);
        let d = compile_err(&p, "greeter", &Args::Positional(vec![Value::string("bob")]));
        assert_eq!(d.code, E003);
    }

    #[test]
    fn bare_dollar_name_fallback_consults_own_scope_only() {
        let p = project_with(&[("main.yml", "main: \"n=$x\"\n"), ("a/b.yml", "x: 7\n")]);
        // The fallback resolves from the referencing component's file: the
        // sub-namespace `x` is NOT visible from main.yml.
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E003);
        // But `subdir.x`-style qualified names work via math, not bare `$name`.
        let opts = Options {
            plain: PlainMode::All,
            ..Options::default()
        };
        assert_eq!(
            compile_component(&p, "main", &Args::None, &opts).unwrap(),
            Value::string("n=7"),
            "PlainMode::All promotes the sub-namespace component"
        );
    }

    #[test]
    fn bare_dollar_name_fallback_resolves_file_scoped() {
        let p = project_with(&[("main.yml", "_secret: 41\nmain: \"v=$_secret\"\n")]);
        assert_eq!(compile_ok(&p, "main", &Args::None), Value::string("v=41"));
        // A file-scoped name from another file is not visible.
        let p = project_with(&[
            ("main.yml", "main: \"v=$_secret\"\n"),
            ("a/b.yml", "_secret: 41\nb: 1\n"),
        ]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E003);
    }

    #[test]
    fn no_fallback_inside_math_context() {
        let p = project_with(&[("main.yml", "x: 5\nmain: \"${x}\"\n")]);
        // PRD *String syntax*: math bare identifiers have no component
        // fallback; `x` is neither an argument nor `last` -> E003.
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("x"), "{}", d.message);
    }

    #[test]
    fn integer_keys_are_positional_slots() {
        let p = project_with(&[("main.yml", "main:\n  0: hello\n  name: $0\n")]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("hello")),
                ("name".to_string(), Value::string("hello")),
            ])),
            "slots appear stringified and named props read the slot default"
        );
        assert_eq!(
            compile_ok(&p, "main", &Args::Positional(vec![Value::string("x")])),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("x")),
                ("name".to_string(), Value::string("x")),
            ])),
            "the call's positional argument overwrites the slot"
        );
    }

    #[test]
    fn slots_are_defaults_for_missing_positionals_only() {
        let p = project_with(&[(
            "main.yml",
            "main:\n  0: d0\n  1: d1\n  2: d2\n  out: \"$0/$1/$2\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("d0")),
                ("1".to_string(), Value::string("d1")),
                ("2".to_string(), Value::string("d2")),
                ("out".to_string(), Value::string("d0/d1/d2")),
            ]))
        );
        assert_eq!(
            compile_ok(&p, "main", &Args::Positional(vec![Value::string("a")])),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("a")),
                ("1".to_string(), Value::string("d1")),
                ("2".to_string(), Value::string("d2")),
                ("out".to_string(), Value::string("a/d1/d2")),
            ])),
            "only the provided index is overwritten"
        );
        assert_eq!(
            compile_ok(
                &p,
                "main",
                &Args::Mixed {
                    named: vec![("k".to_string(), Value::int(9))],
                    positional: vec![Value::string("a"), Value::string("b")],
                }
            ),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("a")),
                ("1".to_string(), Value::string("b")),
                ("2".to_string(), Value::string("d2")),
                ("out".to_string(), Value::string("a/b/d2")),
            ]))
        );
    }

    #[test]
    fn slot_defaults_resolve_against_the_call_scope() {
        let p = project_with(&[("main.yml", "main:\n  0: \"hi $n\"\n  out: $0\n")]);
        assert_eq!(
            compile_ok(&p, "main", &named(&[("n", Value::string("bob"))])),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("hi bob")),
                ("out".to_string(), Value::string("hi bob")),
            ]))
        );
    }

    #[test]
    fn string_zero_and_negative_keys_are_ordinary() {
        let p = project_with(&[(
            "main.yml",
            "main:\n  0: slot\n  \"0\": named\n  -1: neg\n  x: 1\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("slot")),
                ("-1".to_string(), Value::string("neg")),
                ("x".to_string(), Value::int(1)),
            ])),
            "duplicate output keys drop the later occurrence (first wins)"
        );
    }

    #[test]
    fn slot_keys_keep_source_order_in_output() {
        let p = project_with(&[("main.yml", "main:\n  a: 1\n  0: z\n  b: 2\n")]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                ("a".to_string(), Value::int(1)),
                ("0".to_string(), Value::string("z")),
                ("b".to_string(), Value::int(2)),
            ]))
        );
    }

    #[test]
    fn nested_objects_have_no_slot_semantics() {
        let p = project_with(&[("main.yml", "main:\n  a:\n    0: x\n    1: y\n")]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([(
                "a".to_string(),
                Value::object(IndexMap::from([
                    ("0".to_string(), Value::string("x")),
                    ("1".to_string(), Value::string("y")),
                ])),
            )]))
        );
    }

    #[test]
    fn oversized_slot_key_is_e010() {
        let p = project_with(&[("main.yml", "main:\n  65536: x\n")]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E010);
        assert!(d.message.contains("slot"), "{}", d.message);
    }

    // ---- Milestone 1.6 task 3: inline call-sites (rule 3) ----

    #[test]
    fn inline_call_site_binds_positional_and_named_args() {
        let p = project_with(&[(
            "main.yml",
            "sum: \"$0 + $1\"\ngreet: \"hi $name\"\nmain: [\"$sum(12, 34)\", \"$greet(name=Bob)\"]\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::array(vec![Value::string("12 + 34"), Value::string("hi Bob")])
        );
    }

    #[test]
    fn inline_call_site_no_args_and_bypass() {
        let p = project_with(&[(
            "main.yml",
            "five: 5\nx: 42\nvia_arg: \"$x\"\nvia_call: \"$x()\"\nmain: \"$five()\"\n",
        )]);
        assert_eq!(compile_ok(&p, "main", &Args::None), Value::int(5));
        // `$name(...)` bypasses the argument lookup (rule 2): the component
        // wins even with an in-scope argument of the same name.
        assert_eq!(
            compile_ok(&p, "via_call", &named(&[("x", Value::string("arg"))])),
            Value::int(42)
        );
        assert_eq!(
            compile_ok(&p, "via_arg", &named(&[("x", Value::string("arg"))])),
            Value::string("arg"),
            "bare `$x` keeps the rule-2 argument-first order"
        );
    }

    #[test]
    fn inline_call_site_argument_values() {
        let p = project_with(&[(
            "main.yml",
            "echo: $0\nmain: [\"$echo(null)\", \"$echo(~)\", \"$echo(true)\", \"$echo(12)\", \"$echo(-3)\", \"$echo(1.5)\", \"$echo(abc)\", \"$echo('s')\", \"$echo(\\\"t\\\")\"]\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::array(vec![
                Value::null(),
                Value::null(),
                Value::bool(true),
                Value::int(12),
                Value::int(-3),
                Value::float(1.5),
                Value::string("abc"),
                Value::string("s"),
                Value::string("t"),
            ])
        );
    }

    #[test]
    fn inline_call_site_nested_calls_refs_and_math() {
        let p = project_with(&[(
            "main.yml",
            "id: $0\nsix: 6\nfive: 5\nmain: [\"$id($id($five()))\", \"$id(${1+2})\", \"$id($0)\", \"$id($n)\", \"$id($six)\"]\n",
        )]);
        assert_eq!(
            compile_ok(
                &p,
                "main",
                &Args::Mixed {
                    named: vec![("n".to_string(), Value::int(7))],
                    positional: vec![Value::int(9)],
                }
            ),
            Value::array(vec![
                Value::int(5),
                Value::int(3),
                Value::int(9),
                Value::int(7),
                Value::int(6),
            ])
        );
    }

    #[test]
    fn inline_call_site_slot_defaults_apply() {
        let p = project_with(&[(
            "main.yml",
            "comp:\n  0: d\n  out: $0\nmain: \"$comp(x)\"\nwith_default: \"$comp()\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("x")),
                ("out".to_string(), Value::string("x")),
            ])),
            "call-site positional args set the callee's slots"
        );
        assert_eq!(
            compile_ok(&p, "with_default", &Args::None),
            Value::object(IndexMap::from([
                ("0".to_string(), Value::string("d")),
                ("out".to_string(), Value::string("d")),
            ])),
            "no positional arg keeps the slot default"
        );
    }

    #[test]
    fn inline_call_site_missing_target_is_e002() {
        let p = project_with(&[("main.yml", "main: \"$nope(1)\"\n")]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E002);
        assert_eq!(d.component.as_deref(), Some("nope"), "{}", d.message);
        assert!(d.message.contains("nope"), "{}", d.message);
    }

    #[test]
    fn inline_call_site_cross_file_scoped_target_is_e005() {
        let p = project_with(&[
            ("main.yml", "main: \"$_s(1)\"\n"),
            ("a/b.yml", "_s: 1\nb: 2\n"),
        ]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E005);
        assert!(d.message.contains("_s"), "{}", d.message);
    }

    #[test]
    fn inline_call_site_grammar_errors() {
        let p = project_with(&[
            ("main.yml", "id: $0\nmain: \"$id(a=1, 2)\"\n"),
            ("arr.yml", "arr: \"$id([1,2])\"\n"),
            ("unterm.yml", "unterm: \"$id(\"\n"),
            ("esc.yml", "esc: \"$id(\\\"a\\\\qb\\\")\"\n"),
        ]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E012, "positional after named");
        let d = compile_err(&p, "arr", &Args::None);
        assert_eq!(d.code, E013, "array literal as direct arg");
        let d = compile_err(&p, "unterm", &Args::None);
        assert_eq!(d.code, E010, "unterminated call-site");
        let d = compile_err(&p, "esc", &Args::None);
        assert_eq!(d.code, E010, "invalid escape in quoted arg");
    }

    #[test]
    fn non_call_site_strings_stay_interpolated() {
        let p = project_with(&[("main.yml", "main: \"$x(1)!\"\n")]);
        assert_eq!(
            compile_ok(&p, "main", &named(&[("x", Value::int(5))])),
            Value::string("5(1)!"),
            "trailing text after the parens is not a call-site"
        );
        let p = project_with(&[("main.yml", "main: \"$$box(1)\"\n")]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(
            d.code, E010,
            "templates are not inline-callable (dangling `$` in scan)"
        );
    }

    #[test]
    fn inline_call_site_callee_missing_arg_is_e003() {
        let p = project_with(&[("main.yml", "comp: \"$0\"\nmain: \"$comp()\"\n")]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("$0"), "{}", d.message);
    }
}
