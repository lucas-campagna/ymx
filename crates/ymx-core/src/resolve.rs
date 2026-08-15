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

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::IndexMap;

use crate::callsite;
use crate::diag::{Diagnostic, FileId, Span, E002, E005, E006, E008, E009, E010};
use crate::interp;
use crate::ir::{Args, Value};
use crate::math::{CallHook, FallbackHook, Scope, V1Engine};
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
        .call_root(def, args, None)
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
        .call_root(def, &Args::None, None)
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
    depth: Cell<u32>,
}

impl<'a> Resolver<'a> {
    fn new(project: &'a Project, opts: &'a Options) -> Resolver<'a> {
        Resolver {
            project,
            opts,
            depth: Cell::new(0),
        }
    }

    /// Resolve `def` as a normal component call with `args`. Milestone 1.6
    /// task 3: the rule-11 pipeline is step 1 (property resolution incl. the
    /// rule-4 slots, the rule-2 fallback, and rule-3 inline call-sites)
    /// followed by the output conversion; the template chain (task 5) and
    /// `from`/shortcut dispatch (tasks 6–7) slot in around it.
    ///
    /// Invariant #6 (task 9): every recursive operation — an inline
    /// `$comp(...)` call, a math `comp(...)` call, a bare-`$name` component
    /// fallback, a template step, or a `from` dispatch — checks the depth
    /// counter **before** incrementing: at `depth == max_depth` the operation
    /// aborts with `E008`; otherwise the counter is bumped for the duration
    /// of the operation and restored on the way out, so at most `max_depth`
    /// recursive operations are allowed per compilation (default 256). All
    /// five operations invoke a component call, so the guard lives here; the
    /// top-level `compile` / `compile_component` entries use the unguarded
    /// [`Resolver::call_root`] and do not consume a slot.
    fn call(
        &self,
        def: &Definition,
        args: &Args,
        chain_initial: Option<&Args>,
    ) -> Result<Value, Diagnostic> {
        let depth = self.depth.get();
        if depth == self.opts.max_depth {
            return Err(self.def_err(
                def,
                E008,
                format!("max recursion depth ({}) exceeded", self.opts.max_depth),
            ));
        }
        self.depth.set(depth + 1);
        let result = self.call_root(def, args, chain_initial);
        self.depth.set(depth);
        result
    }

    fn call_root(
        &self,
        def: &Definition,
        args: &Args,
        chain_initial: Option<&Args>,
    ) -> Result<Value, Diagnostic> {
        let scope = self.scope_for(def, args);
        let body = self.resolve_body(&def.body, &scope, def.file)?;
        self.finish(def, args, chain_initial, body)
    }

    /// Rule-11 steps 2–3 on the post-step-1 body. Step 2 (rule 5): the
    /// template chain — `def`'s output feeds `$<name>`, whose own output
    /// feeds the next link, each link a normal component call; a broken
    /// link stops the chain. Step 3 (`from`/shortcut dispatch, rules 6/8)
    /// slots in here in tasks 6–7. `chain_initial` is the chain's initial
    /// args (what the chain's first link received) when this call is itself
    /// a chain link, else `args` (a fresh chain origin).
    fn finish(
        &self,
        def: &Definition,
        args: &Args,
        chain_initial: Option<&Args>,
        body: ResolvedBody,
    ) -> Result<Value, Diagnostic> {
        let initial = chain_initial.unwrap_or(args);
        let output = self.output(&body);
        // Step 2: the template chain (rule 5).
        let chained = self.chain_link(def, initial, &output, chain_initial)?;
        // Step 3: `from` / shortcut dispatch (rules 6/8) on the post-chain
        // property set.
        match chained {
            Some(result) => self.step3(&result, def.file, Some(&def.full_name)),
            None => match &body {
                ResolvedBody::Object(props) => {
                    self.dispatch_from(props, def.file, Some(&def.full_name))
                }
                ResolvedBody::Value(v) => self.step3(v, def.file, Some(&def.full_name)),
            },
        }
    }

    /// Rule-11 step 3 — `from` / shortcut dispatch (rules 6/8). Runs on the
    /// post-chain output of a normal component call: an object whose `from`
    /// names a valid regular component dispatches — the target is called
    /// with the rest of the property set as arguments (the `from` key
    /// itself is not forwarded, and the rule-8 shortcut is suppressed);
    /// any other object (no `from`, non-String `from`, missing target,
    /// template target) runs the rule-8 shortcut on its properties. Non-
    /// object outputs pass through.
    fn step3(
        &self,
        value: &Value,
        file: FileId,
        component: Option<&str>,
    ) -> Result<Value, Diagnostic> {
        let Value::Object(m) = value else {
            return Ok(value.clone());
        };
        match self.resolve_from_target(m, file)? {
            Some(def) => {
                let named: Vec<(String, Value)> = m
                    .iter()
                    .filter(|(k, _)| *k != &self.opts.from_keyword)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                self.call(def, &args_from(named, Vec::new()), None)
            }
            None => match self.shortcut(m, &[], file, component)? {
                Some(result) => Ok(result),
                None => Ok(value.clone()),
            },
        }
    }

    /// Rule-8 shortcut (step 3, sugar for `from`; mutually exclusive): a
    /// property whose name resolves to a regular component — global +
    /// `plain` promotion, file-scoped `_`-prefixed names included;
    /// templates never match — calls that component with the property's
    /// value bound to `opts.default_keyword` (e.g. `default`) and the
    /// remaining properties as arguments (integer-keyed slots become
    /// positional args). More than one match is the ambiguous-shortcut
    /// `E006`; no match leaves the object untouched. An invalid `from` key
    /// does not match — it forwards as an ordinary argument alongside the
    /// rest.
    fn shortcut(
        &self,
        named: &IndexMap<String, Value>,
        slots: &[Value],
        file: FileId,
        component: Option<&str>,
    ) -> Result<Option<Value>, Diagnostic> {
        let mut matches: Vec<(&str, &'a Definition, &Value)> = named
            .iter()
            .filter(|(k, _)| *k != &self.opts.from_keyword)
            .filter_map(|(k, v)| {
                match resolve_ref(self.project, k, file, self.opts.plain.clone()) {
                    Ok(def) if !def.full_name.starts_with('$') => Some((k.as_str(), def, v)),
                    _ => None,
                }
            })
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => {
                let (key, def, value) = matches.pop().unwrap();
                let mut args = Vec::with_capacity(named.len() + 1);
                args.push((self.opts.default_keyword.clone(), value.clone()));
                for (k, v) in named {
                    if k != key {
                        args.push((k.clone(), v.clone()));
                    }
                }
                let call_args = args_from(args, slots.to_vec());
                Ok(Some(self.call(def, &call_args, None)?))
            }
            _ => {
                let names: Vec<&str> = matches.iter().map(|(k, _, _)| *k).collect();
                Err(Diagnostic {
                    file: Some(self.project.files[file.0 as usize].clone()),
                    line: 1,
                    col: 1,
                    component: component.map(str::to_string),
                    code: E006,
                    message: format!(
                        "ambiguous shortcut: properties {} all match components",
                        names.join(", ")
                    ),
                })
            }
        }
    }

    /// Rule-11 step 2: the next template-chain link. The chain name is one
    /// `$` longer (`a` → `$a` → `$$a` → …), looked up in `def`'s own
    /// namespace first, then the global namespace with `plain` promotion
    /// (file-scoped `_`-prefixed templates included); a missing link stops
    /// the chain (no skip to the next `$`). The link is a normal component
    /// call (its own three-step flow): its args derive from the chain's
    /// initial args, so an overwrite lasts exactly one step and reverts
    /// (rule 5); `chain_initial` is threaded down unchanged (for a fresh
    /// origin the first derivation defines it). Only the **first** link of
    /// a chain (per the rule-11 step-2 exception) may use array semantics:
    /// an array component output through a non-array-bodied template is a
    /// rule-12 map ([`Resolver::map_over`]); an array-bodied template is a
    /// rule-13 reduce ([`Resolver::reduce_over`]) over a non-array
    /// component output, while an array-bodied template reached anywhere
    /// else in a chain is the mixed-shape `E010` (rules 12–14).
    fn chain_link(
        &self,
        def: &Definition,
        initial: &Args,
        result: &Value,
        chain_initial: Option<&Args>,
    ) -> Result<Option<Value>, Diagnostic> {
        let ns = self.def_namespace(def);
        let name = format!("${}", def.full_name);
        let Some(tpl) = self.lookup_template(&ns, &name, def.file) else {
            return Ok(None);
        };
        if matches!(tpl.body, Node::Array(..)) {
            if chain_initial.is_some() {
                return Err(self.def_err(
                    tpl,
                    E010,
                    format!(
                        "array-bodied template `{}` reached outside the first link of a template chain (mixed-shape chain; only the first link may use array semantics)",
                        tpl.full_name
                    ),
                ));
            }
            return Ok(Some(self.reduce_over(tpl, result)?));
        }
        if chain_initial.is_none() && matches!(result, Value::Array(_)) {
            return Ok(Some(self.map_over(tpl, result)?));
        }
        let link_args = self.derive_chain_args(initial, result, def)?;
        let threaded = chain_initial.unwrap_or(&link_args);
        Ok(Some(self.call(tpl, &link_args, Some(threaded))?))
    }

    /// Rule 12 (map): a non-array-bodied `$template` over an array
    /// component output — each item of the array passes through the
    /// template body, producing one output item per input item. Object
    /// items bind their properties as named args (the PRD map examples);
    /// any other item binds `$0`. An empty array maps to an empty array.
    /// Each item evaluation is a template step: the rule-11 three-step
    /// flow minus the template chain (an empty chain), consuming one
    /// depth slot.
    fn map_over(&self, tpl: &Definition, result: &Value) -> Result<Value, Diagnostic> {
        let Value::Array(items) = result else {
            unreachable!("rule-12 map requires an array component output")
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let args = item_args(item);
            out.push(self.resolve_array_step(tpl, &tpl.body, &args, None)?);
        }
        Ok(Value::Array(out))
    }

    /// Rule 13 (reduce): an array-bodied `$template` over a non-array
    /// component output iterates the template's own items, the component
    /// supplying the initial arguments. Each step starts from the initial
    /// args; the previous step's result overwrites them **only for the
    /// keys it actually returns, only for the immediately-next step** (a
    /// non-object result reverts to the initial args entirely — no `$0`
    /// overwrite); the previous step's result is exposed as `last` from
    /// step 2 onward. The final result of the whole reduce is the last
    /// step's result. An empty `$template` is a pass-through (input
    /// unchanged). An array-bodied template over an array component
    /// output is rule 14 (milestone 1.7 task 3), still `E010` here.
    fn reduce_over(&self, tpl: &Definition, result: &Value) -> Result<Value, Diagnostic> {
        let Node::Array(items, _) = &tpl.body else {
            unreachable!("rule-13 reduce requires an array-bodied template")
        };
        if items.is_empty() {
            return Ok(result.clone());
        }
        match result {
            Value::Array(_) => Err(self.def_err(
                tpl,
                E010,
                format!(
                    "array-bodied template `{}` over an array component requires rule 14 (milestone 1.7)",
                    tpl.full_name
                ),
            )),
            other => self.reduce_run(tpl, items, &item_args(other)),
        }
    }

    /// One rule-13 reduce run over the template's items with `initial`
    /// args. `last` is threaded step to step; the step args for step
    /// *i+1* derive from step *i*'s result (an object overwrites the
    /// returned keys over `initial`; anything else leaves `initial`
    /// unchanged).
    fn reduce_run(
        &self,
        tpl: &Definition,
        items: &[Node],
        initial: &Args,
    ) -> Result<Value, Diagnostic> {
        let mut step_args = initial.clone();
        let mut prev: Option<Value> = None;
        for item in items {
            let result = self.resolve_array_step(tpl, item, &step_args, prev.as_ref())?;
            step_args = match &result {
                Value::Object(m) => overwrite_named_args(initial, m),
                _ => initial.clone(),
            };
            prev = Some(result);
        }
        Ok(prev.expect("a non-empty reduce always produces a result"))
    }

    /// One array-template step (a rule-12 map item or a rule-13/14 reduce
    /// step): the item node runs through the rule-11 three-step flow minus
    /// the template chain (an empty chain) against a scope built from
    /// `args` (and the previous reduce step's result as `last`, when
    /// given). The step is a recursive op like any template step: it
    /// checks the depth cap at entry (`E008` at the boundary) and consumes
    /// one slot (invariant #6).
    fn resolve_array_step(
        &self,
        tpl: &Definition,
        item: &Node,
        args: &Args,
        last: Option<&Value>,
    ) -> Result<Value, Diagnostic> {
        let depth = self.depth.get();
        if depth == self.opts.max_depth {
            return Err(self.def_err(
                tpl,
                E008,
                format!("max recursion depth ({}) exceeded", self.opts.max_depth),
            ));
        }
        self.depth.set(depth + 1);
        let result = (|| {
            let mut scope = self.scope_for(tpl, args);
            scope.last = last.cloned();
            let body = self.resolve_body(item, &scope, tpl.file)?;
            match &body {
                ResolvedBody::Object(props) => {
                    self.dispatch_from(props, tpl.file, Some(&tpl.full_name))
                }
                ResolvedBody::Value(v) => self.step3(v, tpl.file, Some(&tpl.full_name)),
            }
        })();
        self.depth.set(depth);
        result
    }

    /// Rule-5 chain lookup: the component's own namespace first, then the
    /// global namespace with `plain` promotion (via [`resolve_ref`]).
    fn lookup_template(&self, ns: &str, name: &str, file: FileId) -> Option<&'a Definition> {
        self.project
            .namespaces
            .get(ns, name)
            .or_else(|| resolve_ref(self.project, name, file, self.opts.plain.clone()).ok())
    }

    /// Rule-5 argument passing between chain steps: the next link's args
    /// derive from the chain origin's initial args — a scalar result
    /// overwrites `$0` (other initial args retained); an object result
    /// overwrites only the returned keys for this one step (new keys are
    /// added for the step); an array result in a non-array chain is the v1
    /// mixed-shape `E010` (rules 12–14).
    fn derive_chain_args(
        &self,
        initial: &Args,
        result: &Value,
        def: &Definition,
    ) -> Result<Args, Diagnostic> {
        match result {
            Value::Object(m) => {
                let mut named = initial.named_vec();
                for (k, v) in m {
                    match named.iter_mut().find(|(nk, _)| nk == k) {
                        Some(slot) => slot.1 = v.clone(),
                        None => named.push((k.clone(), v.clone())),
                    }
                }
                Ok(args_from(named, initial.positional_vec()))
            }
            v if v.is_scalar() => {
                let mut positional = initial.positional_vec();
                if positional.is_empty() {
                    positional.push(v.clone());
                } else {
                    positional[0] = v.clone();
                }
                Ok(args_from(initial.named_vec(), positional))
            }
            _ => Err(self.def_err(
                def,
                E010,
                "array result in a non-array template chain (mixed-shape chain; rules 12–14 are milestone 1.7)"
                    .to_string(),
            )),
        }
    }

    /// The namespace of `def`'s hosting document: its directory as a dotted
    /// string (`""` for root files), mirroring `load_project` naming.
    fn def_namespace(&self, def: &Definition) -> String {
        let full = &self.project.files[def.file.0 as usize];
        let rel = full.strip_prefix(&self.project.root).unwrap_or(full);
        rel.parent()
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .replace('/', ".")
    }

    /// A diagnostic attributed to `def`'s key span.
    fn def_err(&self, def: &Definition, code: &'static str, message: String) -> Diagnostic {
        Diagnostic {
            file: Some(self.project.files[def.file.0 as usize].clone()),
            line: def.span.line,
            col: def.span.col,
            component: Some(def.full_name.clone()),
            code,
            message,
        }
    }

    /// The rule-11 output value for a post-step-1 body.
    fn output(&self, body: &ResolvedBody) -> Value {
        match body {
            ResolvedBody::Value(v) => v.clone(),
            ResolvedBody::Object(set) => set.to_object(),
        }
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
        let call: CallHook<'s> = {
            let file = def.file;
            let span = def.span;
            Rc::new(move |name: &str, args: &[Value]| {
                match resolve_ref(self.project, name, file, self.opts.plain.clone()) {
                    Ok(target) => self.call(target, &Args::Positional(args.to_vec()), None),
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
            })
        };
        Scope {
            file: Some(self.project.files[def.file.0 as usize].clone()),
            component: Some(def.full_name.clone()),
            span: def.span,
            named: args.named_vec(),
            positional: args.positional_vec(),
            last: None,
            call: Some(call),
            fallback: Some(fallback),
        }
    }

    /// Rule-2 fallback (b): a regular component `name` reachable from `file`
    /// is called with no args. `NotFound` / file-scope violations yield
    /// `Ok(None)` so the caller reports the plain `E003`.
    fn lookup_component(&self, file: FileId, name: &str) -> Result<Option<Value>, Diagnostic> {
        match resolve_ref(self.project, name, file, self.opts.plain.clone()) {
            Ok(def) => Ok(Some(self.call(def, &Args::None, None)?)),
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
        self.reject_modifier_keys(entries, scope)?;
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
                self.reject_modifier_keys(entries, scope)?;
                if entries.iter().any(|e| self.is_from_key(&e.key)) {
                    return self.resolve_mini(entries, scope, file);
                }
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

    /// True when `key` is the `from` keyword (per `opts.from_keyword`).
    fn is_from_key(&self, key: &crate::parse::Key) -> bool {
        matches!(key, crate::parse::Key::String(s) if s == &self.opts.from_keyword)
    }

    /// v1 rejection of the v2 property-key modifiers (rules 17/18): a string
    /// key carrying a trailing `?` or `$` is `E010` when reached during
    /// step 1. The v2 strip / `${...}`-wrap / `?:`-default semantics are
    /// deliberately not implemented.
    fn reject_modifier_keys(
        &self,
        entries: &[crate::parse::Entry],
        scope: &Scope<'_>,
    ) -> Result<(), Diagnostic> {
        for entry in entries {
            if let crate::parse::Key::String(s) = &entry.key {
                if s.ends_with('?') || s.ends_with('$') {
                    return Err(ctx_err(
                        scope,
                        E010,
                        format!("property-key modifier `{s}` is not supported in v1"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// A nested mini-component (rule 11): an object value containing the
    /// `from` key. Its explicitly written properties — `from` excluded — are
    /// the arguments to the `from` target, resolved against the parent's
    /// scope with the same step-1 rules (slots included: `0: $x` binds `$0`).
    /// The mini's `from` dispatch (rule-11 step 3) runs here: a valid
    /// regular-component target is called and its return value replaces the
    /// object; an invalid `from` (non-String, missing target, template) is
    /// forwarded as a plain property (the rule-8 shortcut joins in task 7).
    fn resolve_mini(
        &self,
        entries: &[crate::parse::Entry],
        scope: &Scope<'_>,
        file: FileId,
    ) -> Result<Value, Diagnostic> {
        let props = match self.resolve_property_set(entries, scope, file)? {
            ResolvedBody::Object(props) => props,
            ResolvedBody::Value(_) => {
                unreachable!("an object body always resolves to a property set")
            }
        };
        self.dispatch_from(&props, file, scope.component.as_deref())
    }

    /// Rule-11 step-3 `from` / shortcut dispatch on a resolved property set,
    /// shared by mini-components and the top-level no-chain pipeline: a
    /// valid `from` target is called with the rest of the property set
    /// (slots included) as arguments; an invalid `from` (or none) forwards
    /// the object unchanged unless the rule-8 shortcut matches a property,
    /// in which case that call replaces the object.
    fn dispatch_from(
        &self,
        props: &PropertySet,
        file: FileId,
        component: Option<&str>,
    ) -> Result<Value, Diagnostic> {
        match self.resolve_from_target(&props.named, file)? {
            Some(def) => {
                let args = self.props_to_args(props);
                self.call(def, &args, None)
            }
            None => match self.shortcut(&props.named, &props.slots, file, component)? {
                Some(result) => Ok(result),
                None => Ok(props.to_object()),
            },
        }
    }

    /// The valid `from` dispatch target of a resolved object: the `from`
    /// value must be a String naming a regular (non-template) component
    /// reachable from `file` — dotted `subdir.comp` included. `None` means
    /// `from` is invalid (absent, non-String, missing target, template) and
    /// forwards as a plain property.
    fn resolve_from_target(
        &self,
        named: &IndexMap<String, Value>,
        file: FileId,
    ) -> Result<Option<&'a Definition>, Diagnostic> {
        let Some(from) = named.get(&self.opts.from_keyword) else {
            return Ok(None);
        };
        let Some(target) = (match from {
            Value::String(s) => Some(s),
            _ => None,
        }) else {
            return Ok(None);
        };
        match resolve_ref(self.project, target, file, self.opts.plain.clone()) {
            Ok(def) if !def.full_name.starts_with('$') => Ok(Some(def)),
            _ => Ok(None),
        }
    }

    /// The call-site-style [`Args`] for a resolved property set: the named
    /// properties in source order (`from` excluded) plus the positional
    /// slots.
    fn props_to_args(&self, props: &PropertySet) -> Args {
        let named: Vec<(String, Value)> = props
            .named
            .iter()
            .filter(|(k, _)| *k != &self.opts.from_keyword)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        args_from(named, props.slots.clone())
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
            Ok(def) => self.call(def, args, None),
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

/// Build [`Args`] from (named, positional) parts, choosing the minimal
/// variant.
fn args_from(named: Vec<(String, Value)>, positional: Vec<Value>) -> Args {
    match (named.is_empty(), positional.is_empty()) {
        (true, true) => Args::None,
        (false, true) => Args::Named(named),
        (true, false) => Args::Positional(positional),
        (false, false) => Args::Mixed { named, positional },
    }
}

/// The [`Args`] an array-template step passes to the template for one
/// input item: an object item binds its properties as named args (rule
/// 12's map examples), anything else binds `$0` (rule 13's scalar `a`
/// case and rule 14's scalar elements).
fn item_args(item: &Value) -> Args {
    match item {
        Value::Object(m) => args_from(
            m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Vec::new(),
        ),
        _ => Args::Positional(vec![item.clone()]),
    }
}

/// The rule-13/14 overwrite: `initial` with the object result's returned
/// keys overwritten — keys the result returns that were not in the
/// initial args are added for that one step; positional args are
/// untouched. The initial args themselves are never mutated (each step
/// starts from them afresh).
fn overwrite_named_args(initial: &Args, result: &IndexMap<String, Value>) -> Args {
    let mut named = initial.named_vec();
    for (k, v) in result {
        match named.iter_mut().find(|(nk, _)| nk == k) {
            Some(slot) => slot.1 = v.clone(),
            None => named.push((k.clone(), v.clone())),
        }
    }
    args_from(named, initial.positional_vec())
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

    // ---- Milestone 1.6 task 4: bottom-up property resolution (rule 11
    // step 1): nested mini-components, inline calls before templates, and
    // the v1 E010 rejection of `?`/`$` property-key modifiers ----

    #[test]
    fn mini_component_dispatches_from_target() {
        let p = project_with(&[(
            "main.yml",
            "comp: \"v=$x\"\nmain:\n  mini: {from: comp, x: 1}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([(
                "mini".to_string(),
                Value::string("v=1")
            )])),
            "a property object with `from` becomes a nested call whose written props are the target's args"
        );
    }

    #[test]
    fn mini_component_from_key_is_not_forwarded_as_arg() {
        let p = project_with(&[(
            "main.yml",
            "comp: \"$from\"\nmain:\n  mini: {from: comp, x: 1}\n",
        )]);
        let d = compile_err(&p, "main", &Args::None);
        assert_eq!(
            d.code, E003,
            "the callee must not see the `from` key as an argument"
        );
    }

    #[test]
    fn mini_component_slot_usage() {
        let p = project_with(&[(
            "main.yml",
            "comp: \"hi $0\"\nmain:\n  mini: {from: comp, 0: \"$name\"}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &named(&[("name", Value::string("bob"))])),
            Value::object(IndexMap::from([(
                "mini".to_string(),
                Value::string("hi bob")
            )])),
            "`0: $x` binds the slot against the parent's arguments"
        );
    }

    #[test]
    fn mini_component_from_value_is_a_nested_call_site() {
        let p = project_with(&[(
            "main.yml",
            "b: c\nc: \"${1 + x}\"\nmain:\n  mini: {from: \"$b()\", x: 2}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([("mini".to_string(), Value::int(3))])),
            "example-2 step-1 semantics: `from: $b()` resolves during step 1"
        );
    }

    #[test]
    fn mini_components_resolve_bottom_up() {
        let p = project_with(&[(
            "main.yml",
            "five: 5\nwrap: \"$inner\"\nmain:\n  mini: {from: wrap, inner: {from: five}}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([("mini".to_string(), Value::int(5))])),
            "the inner mini resolves first and its value bubbles up to the outer dispatch"
        );
    }

    #[test]
    fn mini_components_nest_deeply() {
        let p = project_with(&[(
            "main.yml",
            "five: 5\nmid: \"$a\"\ntop: \"$z\"\nmain:\n  mini: {from: top, z: {from: mid, a: {from: five}}}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([("mini".to_string(), Value::int(5))])),
            "example-1 shape: innermost `a`'s mini resolves first, then `z`, then the outer mini"
        );
    }

    #[test]
    fn invalid_mini_from_is_forwarded_as_plain_property() {
        let p = project_with(&[
            ("main.yml", "main:\n  a: {from: 5, x: 1}\n"),
            (
                "t.yml",
                "$box: 1\ntempl:\n  b: {from: '\\$box'}\nmissing:\n  c: {from: nope}\n",
            ),
        ]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([(
                "a".to_string(),
                Value::object(IndexMap::from([
                    ("from".to_string(), Value::int(5)),
                    ("x".to_string(), Value::int(1)),
                ]))
            )])),
            "a non-String `from` is a plain forwarded property"
        );
        assert_eq!(
            compile_ok(&p, "templ", &Args::None),
            Value::object(IndexMap::from([(
                "b".to_string(),
                Value::object(IndexMap::from([(
                    "from".to_string(),
                    Value::string("$box")
                )]))
            )])),
            "a template is not a valid `from` target"
        );
        assert_eq!(
            compile_ok(&p, "missing", &Args::None),
            Value::object(IndexMap::from([(
                "c".to_string(),
                Value::object(IndexMap::from([(
                    "from".to_string(),
                    Value::string("nope")
                )]))
            )])),
            "a missing `from` target is forwarded, not an error"
        );
    }

    #[test]
    fn property_key_modifiers_are_e010_in_v1() {
        for (label, src) in [
            ("top-level", "main:\n  x?: 1\n"),
            ("dollar", "main:\n  x$: 1\n"),
            ("nested", "main:\n  a:\n    b?: 1\n"),
            (
                "mini",
                "comp: 1\nmain:\n  mini:\n    from: comp\n    y$: 2\n",
            ),
        ] {
            let p = project_with(&[("main.yml", src)]);
            let d = compile_err(&p, "main", &Args::None);
            assert_eq!(d.code, E010, "{label}");
        }
        let p = project_with(&[("main.yml", "main:\n  x: 1\n  ok: 2\n")]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([
                ("x".to_string(), Value::int(1)),
                ("ok".to_string(), Value::int(2)),
            ])),
            "unmodified keys are unaffected"
        );
        let p = project_with(&[("main.yml", "a: 1\n$a:\n  x?: 2\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(
            d.code, E010,
            "a template-link body is a step-1 surface: its keys are rejected too"
        );
    }

    // ---- Milestone 1.6 task 5: template chain (rule 5, step 2) ----

    #[test]
    fn template_chain_scalar_links() {
        let p = project_with(&[(
            "main.yml",
            "a: 10\n$a: \"${$0 * 2}\"\n$$a: \"${$0 + 1}\"\n$$$a: \"final: $0\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("final: 21"),
            "the PRD chain example: each link sees the previous result as `$0`"
        );
    }

    #[test]
    fn template_chain_overwrite_then_revert() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n  y: 2\n$a:\n  x: \"${x + 10}\"\n$$a:\n  out: \"$x-$y\"\n$$$a:\n  out: \"$x-$y\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::object(IndexMap::from([("out".to_string(), Value::string("1-2"))])),
            "the overwrite (x=11) lasts exactly one step, then reverts to the initial x=1, y=2"
        );
        // The intermediate step sees the overwrite: assert via a two-link chain.
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n  y: 2\n$a:\n  x: \"${x + 10}\"\n$$a:\n  out: \"$x-$y\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::object(IndexMap::from([("out".to_string(), Value::string("11-2"))])),
            "the immediately-next step sees x overwritten to 11 (PRD `x: 11` example)"
        );
    }

    #[test]
    fn template_chain_object_result_new_keys_for_one_step() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n$a:\n  x: \"${x + 10}\"\n  z: 9\n$$a:\n  out: \"$x-$z\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::object(IndexMap::from([("out".to_string(), Value::string("11-9"))])),
            "a key the link returns that was not in the initial args is added for that one step"
        );
    }

    #[test]
    fn template_chain_scalar_overwrites_dollar_zero_only() {
        let p = project_with(&[("main.yml", "a: 7\n$a: \"$x-$0\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &named(&[("x", Value::string("k"))])),
            Value::string("k-7"),
            "the scalar overwrites `$0`; the initial named args are retained"
        );
    }

    #[test]
    fn template_chain_broken_link_stops() {
        let p = project_with(&[("main.yml", "a: 10\n$$a: \"$0\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(10),
            "missing `$a` breaks the chain; `a` does not skip to `$$a`"
        );
    }

    #[test]
    fn template_chain_namespaced_lookup_own_namespace_first() {
        let p = project_with(&[
            ("main.yml", "x: 5\n$x: \"global\"\nz: 5\n$z: \"global\"\n"),
            ("subdir/x.yml", "x: 5\n$x: \"local\"\n"),
            ("subdir/y.yml", "y: 5\n"),
            ("main2.yml", "$y: \"global\"\n"),
        ]);
        assert_eq!(
            compile_ok(&p, "subdir.x", &Args::None),
            Value::string("local"),
            "the component's own namespace wins over the global template"
        );
        assert_eq!(
            compile_ok(&p, "x", &Args::None),
            Value::string("global"),
            "a global component uses its global template"
        );
        assert_eq!(
            compile_ok(&p, "subdir.y", &Args::None),
            Value::string("global"),
            "a missing own-namespace template falls back to the global one"
        );
    }

    #[test]
    fn template_chain_plain_promotion() {
        let p = project_with(&[
            ("subA/a.yml", "a: 5\n"),
            ("subB/b.yml", "$a: \"promoted\"\n"),
        ]);
        let plain = Options {
            plain: PlainMode::All,
            ..Options::default()
        };
        assert_eq!(
            compile_component(&p, "subA.a", &Args::None, &plain).unwrap(),
            Value::string("promoted"),
            "`plain` promotes the sub-namespace template into global lookup"
        );
        assert_eq!(
            compile_ok(&p, "subA.a", &Args::None),
            Value::int(5),
            "without `plain` the chain is broken at `$a`"
        );
    }

    #[test]
    fn template_chain_file_scoped() {
        let p = project_with(&[("main.yml", "_a: 5\n$_a: \"v=$0\"\n")]);
        assert_eq!(
            compile_ok(&p, "_a", &Args::None),
            Value::string("v=5"),
            "a file-scoped `_`-prefixed chain resolves per-file"
        );
    }

    #[test]
    fn from_dispatch_target_runs_its_own_chain() {
        let p = project_with(&[(
            "main.yml",
            "comp: \"v=$x\"\n$comp: \"T=$0\"\nmain:\n  mini: {from: comp, x: 1}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([(
                "mini".to_string(),
                Value::string("T=v=1")
            )])),
            "a `from` target is a normal component call, so its own template chain applies"
        );
    }

    #[test]
    fn array_bodied_template_over_non_array_component_reduces() {
        let p = project_with(&[("main.yml", "a: 5\n$a:\n  - 1\n  - 2\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(2),
            "an array-bodied `$a` over non-array `a` is a single-element reduce; the final result is the last step's"
        );
    }

    #[test]
    fn array_result_in_non_array_chain_is_e010() {
        // The first link maps (rule 12): a non-array-bodied template over
        // an array component output is not mixed-shape.
        let p = project_with(&[("main.yml", "a: [1, 2]\n$a: \"x\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![Value::string("x"), Value::string("x")]),
            "an origin array result into a non-array-bodied first link maps (rule 12)"
        );
        let p = project_with(&[(
            "main.yml",
            "arr: [1, 2]\na: 5\n$a: \"$arr()\"\n$$a: \"x\"\n",
        )]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E010, "mid-chain array result into a non-array link");
        let p = project_with(&[("main.yml", "a: [1, 2]\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![Value::int(1), Value::int(2)]),
            "an array output with no further chain link is fine"
        );
    }

    // ---- Milestone 1.7 task 1: rule 12 (map) ----

    #[test]
    fn rule12_map_object_items_bind_named_args() {
        let p = project_with(&[(
            "main.yml",
            "$a:\n  prop1: ${x + 1}\n  prop2: ${y * x}\na:\n  - x: 1\n    y: 2\n  - x: 3\n    y: 4\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![
                Value::object(IndexMap::from([
                    ("prop1".to_string(), Value::int(2)),
                    ("prop2".to_string(), Value::int(2)),
                ])),
                Value::object(IndexMap::from([
                    ("prop1".to_string(), Value::int(4)),
                    ("prop2".to_string(), Value::int(12)),
                ])),
            ]),
            "PRD rule-12 example 1: each object item's properties bind the template's named args"
        );
    }

    #[test]
    fn rule12_map_string_template_over_object_array() {
        let p = project_with(&[(
            "main.yml",
            "$a: $x + $y\na:\n  - x: 1\n    y: 2\n  - x: 3\n    y: 4\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![Value::string("1 + 2"), Value::string("3 + 4")]),
            "PRD rule-12 example 2: one output item per input item"
        );
    }

    #[test]
    fn rule12_map_scalar_items_bind_dollar_zero() {
        let p = project_with(&[("main.yml", "$a: \"${$0 * 2}\"\na: [1, 2, 3]\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![Value::int(2), Value::int(4), Value::int(6)]),
            "non-object items bind `$0` per item"
        );
    }

    #[test]
    fn rule12_map_empty_array_outputs_empty_array() {
        let p = project_with(&[("main.yml", "$a: \"v=$x\"\na: []\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![]),
            "an empty array component maps to an empty array (no template call)"
        );
    }

    #[test]
    fn rule12_map_items_run_their_own_three_step_flow() {
        let p = project_with(&[(
            "main.yml",
            "b: \"$default|$x\"\n$a:\n  b: 1\n  x: $x\na:\n  - x: 1\n  - x: 2\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::array(vec![Value::string("1|1"), Value::string("1|2")]),
            "each item step resolves its own body against the item's args and runs step-3 shortcut dispatch"
        );
    }

    #[test]
    fn rule12_map_item_steps_consume_depth_slots() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: [1]\n$a: \"v\"\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::array(vec![Value::string("v")]),
            "one map item step is the first recursive op"
        );
        let p = project_with(&[("main.yml", "a: [1]\n$a: \"$b()\"\nb: 1\n")]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(
            d.code, E008,
            "the item step plus its inline call exceed the cap"
        );
        assert_eq!(d.component.as_deref(), Some("b"));
        let p = project_with(&[("main.yml", "a: []\n$a: \"$b()\"\nb: 1\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::array(vec![]),
            "an empty map consumes no slots"
        );
    }

    // ---- Milestone 1.7 task 2: rule 13 (reduce) ----

    #[test]
    fn rule13_reduce_prd_example() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n  y: 2\n$a:\n  - x: ${x + 1}\n    y: ${y + 2}\n  - ${x + y}\n  - $x + $y < $last\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("1 + 2 < 6"),
            "PRD rule-13 example: step 2 sees the overwritten x/y, step 3 reverts to the initial with `$last` bound"
        );
    }

    #[test]
    fn rule13_overwrite_lasts_exactly_one_step() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n$a:\n  - x: ${x + 10}\n  - ${x}\n  - out: $x\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::object(IndexMap::from([("out".to_string(), Value::int(1))])),
            "step 2 sees the overwritten x=11, step 3 reverts to the initial x=1"
        );
    }

    #[test]
    fn rule13_new_keys_from_object_result_last_one_step() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n$a:\n  - z: 9\n  - ${x + z}\n  - out: $x\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::object(IndexMap::from([("out".to_string(), Value::int(1))])),
            "a key step 1 returns that was not in the initial args is added for step 2 only (`x + z` = 10), then reverts"
        );
    }

    #[test]
    fn rule13_non_object_result_does_not_overwrite_dollar_zero() {
        let p = project_with(&[("main.yml", "a: 5\n$a:\n  - ${$0 + 1}\n  - ${$0 + last}\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(11),
            "step 1's scalar result becomes `last` but does NOT overwrite `$0`: step 2 is 5 + 6, not 6 + 6"
        );
    }

    #[test]
    fn rule13_math_last_available_from_step_two() {
        let p = project_with(&[("main.yml", "a:\n  x: 1\n$a:\n  - ${x}\n  - ${x + last}\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(2),
            "math `last` (bare identifier) is bound from step 2 onward"
        );
    }

    #[test]
    fn rule13_last_on_first_step_is_e003() {
        let p = project_with(&[("main.yml", "a: 1\n$a:\n  - \"$0 + $last\"\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E003, "string `$last` on the first step");
        assert!(d.message.contains("last"), "{}", d.message);
        let p = project_with(&[("main.yml", "a: 1\n$a:\n  - ${last}\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E003, "math `last` on the first step");
        assert!(d.message.contains("last"), "{}", d.message);
    }

    #[test]
    fn rule13_named_arg_last_shadows_reduce_last() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n  last: 5\n$a:\n  - \"$x + $last\"\n  - ${last}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(5),
            "a named argument `last` shadows the reduce `last` in both `$last` and math `last`"
        );
    }

    #[test]
    fn rule13_reduce_steps_run_step3_dispatch() {
        let p = project_with(&[(
            "main.yml",
            "a: 1\nb: \"got=$default\"\n$a:\n  - {b: 7}\n  - ${$0}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(1),
            "an item object with a shortcut-matching property dispatches in step 3, then the scalar result reverts"
        );
    }

    #[test]
    fn rule13_reduce_steps_consume_depth_slots() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: 5\n$a:\n  - v\n  - w\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("w"),
            "each reduce step is one recursive op; the counter is restored between steps"
        );
        let p = project_with(&[("main.yml", "a: 5\n$a:\n  - \"$b()\"\nb: 1\n")]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(
            d.code, E008,
            "a reduce step plus its inline call exceed the cap"
        );
        assert_eq!(d.component.as_deref(), Some("b"));
    }

    // ---- Milestone 1.6 task 6: `from` dispatch (rule 6, step 3) ----

    #[test]
    fn top_level_from_dispatch() {
        let p = project_with(&[(
            "main.yml",
            "CompA: {from: CompB, x: 12, y: 34}\nCompB: \"sum: $x + $y\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "CompA", &Args::None),
            Value::string("sum: 12 + 34"),
            "the target is called with the rest of the property set as arguments"
        );
    }

    #[test]
    fn from_after_template_ordering() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  from: \"$b()\"\n$a:\n  from: $from\n  x: 2\nb: c\nc: \"${1 + x}\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(3),
            "PRD example 2: `from` resolves in step 1, the chain runs in step 2, `from` dispatches in step 3"
        );
    }

    #[test]
    fn from_dispatch_wins_over_same_named_property() {
        let p = project_with(&[(
            "main.yml",
            "comp: \"got=$v\"\nmain:\n  from: comp\n  comp: 5\n  v: 7\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::string("got=7"),
            "a valid `from` dispatches; the same-named property is just an argument (shortcut suppressed)"
        );
    }

    #[test]
    fn namespace_qualified_from() {
        let p = project_with(&[
            ("main.yml", "main:\n  from: subdir.comp\n  x: 1\n"),
            ("subdir/t.yml", "comp: \"v=$x\"\n"),
        ]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::string("v=1"),
            "`from: subdir.comp` resolves through the namespace"
        );
    }

    #[test]
    fn top_level_invalid_from_is_forwarded() {
        let p = project_with(&[
            (
                "main.yml",
                "missing:\n  from: nope\n  x: 1\nnonstr:\n  from: 5\n",
            ),
            ("t.yml", "$box: 1\ntempl:\n  from: '\\$box'\n"),
        ]);
        assert_eq!(
            compile_ok(&p, "missing", &Args::None),
            Value::object(IndexMap::from([
                ("from".to_string(), Value::string("nope")),
                ("x".to_string(), Value::int(1)),
            ])),
            "a missing target forwards `from` as a plain property"
        );
        assert_eq!(
            compile_ok(&p, "nonstr", &Args::None),
            Value::object(IndexMap::from([("from".to_string(), Value::int(5))])),
            "a non-String `from` forwards"
        );
        assert_eq!(
            compile_ok(&p, "templ", &Args::None),
            Value::object(IndexMap::from([(
                "from".to_string(),
                Value::string("$box")
            )])),
            "a template is not a valid `from` target"
        );
    }

    #[test]
    fn top_level_from_dispatch_keeps_slots() {
        let p = project_with(&[(
            "main.yml",
            "main:\n  from: comp\n  0: a\n  1: b\ncomp: \"$0-$1\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::string("a-b"),
            "the slot properties pass through to the `from` target as positional args"
        );
    }

    #[test]
    fn template_link_dispatches_from() {
        let p = project_with(&[("main.yml", "a: 10\n$a:\n  from: c\n  x: 1\nc: \"v=$x\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("v=1"),
            "a chain link is a normal component call, so its own step-3 `from` dispatch applies"
        );
    }

    #[test]
    fn file_scoped_from_target() {
        let p = project_with(&[("main.yml", "_x: 5\nmain:\n  from: _x\n")]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::int(5),
            "a file-scoped component is a valid `from` target within its file"
        );
    }

    // ---- Milestone 1.6 task 7: rule-8 shortcut (step 3, sugar for `from`) ----

    #[test]
    fn shortcut_fires() {
        let p = project_with(&[("main.yml", "a:\n  b: 1\nb: \"${default + 1}\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(2),
            "PRD example 1: the matched property's value is passed as `$default`"
        );
    }

    #[test]
    fn shortcut_suppressed_by_valid_from() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  from: c\n  b: 1\nb: \"${default + 1}\"\nc: \"${b + 2}\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(3),
            "PRD example 2: a valid `from` dispatches and the shortcut does not fire"
        );
    }

    #[test]
    fn invalid_from_forwards_and_shortcut_fires() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  from: nope\n  b: 1\nb: \"$default-$from\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("1-nope"),
            "an invalid `from` is forwarded as an ordinary argument alongside the shortcut call"
        );
    }

    #[test]
    fn ambiguous_shortcut_is_e006() {
        let p = project_with(&[("main.yml", "a:\n  b: 1\n  c: 2\nb: 5\nc: 6\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E006);
        assert!(
            d.message.contains("b") && d.message.contains("c"),
            "{}",
            d.message
        );
    }

    #[test]
    fn template_names_do_not_match_shortcut() {
        let p = project_with(&[("main.yml", "$box: 5\na:\n  \"$box\": 1\n  x: 2\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::object(IndexMap::from([
                ("$box".to_string(), Value::int(1)),
                ("x".to_string(), Value::int(2)),
            ])),
            "a template name never matches the shortcut"
        );
    }

    #[test]
    fn nested_mini_shortcut() {
        let p = project_with(&[(
            "main.yml",
            "b: \"got=$default\"\nmain:\n  mini: {from: nope, b: 7}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([(
                "mini".to_string(),
                Value::string("got=7")
            )])),
            "the shortcut applies inside a mini whose `from` is invalid"
        );
    }

    #[test]
    fn shortcut_passes_slots() {
        let p = project_with(&[(
            "main.yml",
            "a:\n  b: 1\n  0: x\n  1: y\nb: \"$default-$0-$1\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("1-x-y"),
            "integer-keyed properties pass to the shortcut target as positional args"
        );
    }

    #[test]
    fn shortcut_plain_promotion() {
        let p = project_with(&[
            ("subA/a.yml", "a:\n  b: 1\n"),
            ("subB/b.yml", "b: \"got=$default\"\n"),
        ]);
        let plain = Options {
            plain: PlainMode::All,
            ..Options::default()
        };
        assert_eq!(
            compile_component(&p, "subA.a", &Args::None, &plain).unwrap(),
            Value::string("got=1"),
            "`plain` promotion lets the shortcut match a sub-namespace component"
        );
        assert_eq!(
            compile_ok(&p, "subA.a", &Args::None),
            Value::object(IndexMap::from([("b".to_string(), Value::int(1))])),
            "without `plain` the property is not a match"
        );
    }

    #[test]
    fn shortcut_file_scoped() {
        let p = project_with(&[("main.yml", "_b: \"got=$default\"\na:\n  _b: 1\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("got=1"),
            "a file-scoped `_`-prefixed component matches the shortcut within its file"
        );
    }

    #[test]
    fn shortcut_on_post_chain_object() {
        let p = project_with(&[("main.yml", "b: \"got=$default\"\na:\n  x: 1\n$a:\n  b: 2\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("got=2"),
            "the shortcut runs against the post-template property set"
        );
    }

    // ---- Milestone 1.6 task 8: rules 9/10 — unknown props ignored;
    // referenced props required (E003) ----

    #[test]
    fn rule9_unknown_extra_args_are_ignored() {
        let p = project_with(&[("main.yml", "a: \"$x + $y\"\n")]);
        assert_eq!(
            compile_ok(
                &p,
                "a",
                &named(&[
                    ("a", Value::int(1)),
                    ("b", Value::int(2)),
                    ("c", Value::int(3)),
                    ("x", Value::int(1)),
                    ("y", Value::int(2)),
                ])
            ),
            Value::string("1 + 2"),
            "PRD rule-9 example: only the referenced `x` and `y` are read; `a`/`b`/`c` are ignored"
        );
    }

    #[test]
    fn rule10_missing_named_arg_is_e003_with_context() {
        let p = project_with(&[("main.yml", "a: \"v=$x\"\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("`x`"), "{}", d.message);
        assert_eq!(d.component.as_deref(), Some("a"));
        assert!(d.file.is_some(), "E003 carries the resolved file path");
    }

    #[test]
    fn rule10_missing_arg_in_math_is_e003() {
        let p = project_with(&[("main.yml", "a: \"${x + 1}\"\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(
            d.code, E003,
            "a bare identifier inside math is a required argument"
        );
        assert!(d.message.contains("`x`"), "{}", d.message);
    }

    #[test]
    fn rule10_missing_positional_is_e003() {
        let p = project_with(&[("main.yml", "a: \"$0\"\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E003);
        assert!(d.message.contains("`$0`"), "{}", d.message);
        let p = project_with(&[("main.yml", "a: \"$0\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::Positional(vec![Value::int(7)])),
            Value::int(7),
            "the same body is fine once the argument is supplied"
        );
    }

    #[test]
    fn rule9_extra_props_to_dispatch_targets_are_ignored() {
        let p = project_with(&[(
            "main.yml",
            "comp: \"v=$x\"\nmain:\n  mini: {from: comp, x: 1, junk: 2}\n",
        )]);
        assert_eq!(
            compile_ok(&p, "main", &Args::None),
            Value::object(IndexMap::from([("mini".to_string(), Value::string("v=1"))])),
            "the `from` target resolves only the props it references; `junk` is ignored"
        );
    }

    // ---- Milestone 1.6 task 9: depth cap (invariant #6) ----

    fn compile_err_with(p: &Project, component: &str, args: &Args, opts: &Options) -> Diagnostic {
        compile_component(p, component, args, opts)
            .unwrap_err()
            .into_iter()
            .next()
            .expect("at least one diagnostic")
    }

    #[test]
    fn depth_cap_default_permits_deep_chains() {
        let mut src = String::from("a:\n  x: 1\n");
        for i in 1..=200 {
            src.push_str(&format!("{}a: \"v=$x\"\n", "$".repeat(i)));
        }
        let p = project_with(&[("main.yml", &src)]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("v=1"),
            "default max_depth 256 permits a 200-step template chain"
        );
    }

    #[test]
    fn depth_cap_small_max_raises_e008_on_boundary_op() {
        let opts = Options {
            max_depth: 3,
            ..Options::default()
        };
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n$a: \"v=$x\"\n$$a: \"v=$x\"\n$$$a: \"v=$x\"\n",
        )]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("v=1"),
            "exactly max_depth recursive ops are allowed"
        );
        let p = project_with(&[(
            "main.yml",
            "a:\n  x: 1\n$a: \"v=$x\"\n$$a: \"v=$x\"\n$$$a: \"v=$x\"\n$$$$a: \"v=$x\"\n",
        )]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(d.code, E008);
        assert_eq!(d.component.as_deref(), Some("$$$$a"));
        assert!(d.file.is_some(), "E008 carries the resolved file path");
        assert!(d.message.contains("3"), "{}", d.message);
    }

    #[test]
    fn depth_cap_applies_to_inline_calls() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: \"$b()\"\nb: \"v=1\"\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("v=1"),
            "the first recursive op is allowed"
        );
        let p = project_with(&[("main.yml", "a: \"$b()\"\nb: \"$c()\"\nc: \"v=1\"\n")]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(d.code, E008);
        assert_eq!(d.component.as_deref(), Some("c"));
    }

    #[test]
    fn depth_cap_applies_to_bare_name_fallback() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: \"$b\"\nb: \"v=1\"\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("v=1"),
            "the bare-`$name` fallback is the first recursive op"
        );
        let p = project_with(&[("main.yml", "a: \"$b\"\nb: \"$c\"\nc: \"v=1\"\n")]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(d.code, E008);
        assert_eq!(d.component.as_deref(), Some("c"));
    }

    #[test]
    fn depth_cap_applies_to_from_dispatch() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: {from: b, x: 1}\nb: \"v=$x\"\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("v=1"),
            "the `from` dispatch is the first recursive op"
        );
        let p = project_with(&[(
            "main.yml",
            "a: {from: b, x: 1}\nb: {from: c, x: 1}\nc: \"v=$x\"\n",
        )]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(d.code, E008);
        assert_eq!(d.component.as_deref(), Some("c"));
    }

    #[test]
    fn depth_cap_restores_after_e008_for_sibling_calls() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: \"$b()\"\nb: \"$c()\"\nc: \"v=1\"\n")]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(d.code, E008);
        let p = project_with(&[("main.yml", "a: \"$b()\"\nb: \"v=1\"\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("v=1"),
            "the counter is restored after the aborted op"
        );
    }

    // ---- Milestone 1.6 task 9 gap: math `comp(...)` calls (PRD rule 7) ----

    #[test]
    fn math_component_calls_resolve_rule7_example() {
        let p = project_with(&[(
            "main.yml",
            "a: \"${b(12,34) + c(28)}\"\nb: \"${$0 + $1}\"\nc: \"${$0 * 2}\"\n",
        )]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::int(102),
            "PRD rule-7 example: b(12,34) = 46, c(28) = 56, sum 102"
        );
    }

    #[test]
    fn math_component_call_missing_is_e002() {
        let p = project_with(&[("main.yml", "a: \"${nope(1)}\"\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(d.code, E002);
        assert!(d.message.contains("nope"), "{}", d.message);
    }

    #[test]
    fn math_component_call_file_scoped() {
        let p = project_with(&[("main.yml", "a: \"${_b(1)}\"\n_b: \"v=1\"\n")]);
        assert_eq!(
            compile_ok(&p, "a", &Args::None),
            Value::string("v=1"),
            "a `_`-prefixed math call resolves file-scoped from the same document"
        );
        let p = project_with(&[("main.yml", "a: \"${_b(1)}\"\n"), ("other.yml", "_b: 1\n")]);
        let d = compile_err(&p, "a", &Args::None);
        assert_eq!(
            d.code, E005,
            "a cross-document `_` math call is a file-scope violation"
        );
    }

    #[test]
    fn math_component_calls_consume_depth_slots() {
        let opts = Options {
            max_depth: 1,
            ..Options::default()
        };
        let p = project_with(&[("main.yml", "a: \"${b(1)}\"\nb: \"v=1\"\n")]);
        assert_eq!(
            compile_component(&p, "a", &Args::None, &opts).unwrap(),
            Value::string("v=1"),
            "the first math call is the first recursive op"
        );
        let p = project_with(&[("main.yml", "a: \"${b(1)}\"\nb: \"${c(1)}\"\nc: \"v=1\"\n")]);
        let d = compile_err_with(&p, "a", &Args::None, &opts);
        assert_eq!(d.code, E008, "the boundary op is a math component call");
        assert_eq!(d.component.as_deref(), Some("c"));
    }
}
