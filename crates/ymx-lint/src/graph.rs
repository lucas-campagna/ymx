use std::collections::{BTreeMap, BTreeSet};
use std::path::Component;

use ymx_core::builtin::Builtin;
use ymx_core::diag::{FileId, Span};
use ymx_core::ir::Value;
use ymx_core::namespace::{classify, DefClass, Definition};
use ymx_core::project::{Options, PlainMode, Project};
use ymx_core::refs::{self, ArgRef, NodeRef, Slot};
use ymx_core::resolve::resolve_entry;

pub(crate) type DefId = usize;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SlotKey {
    Named(String),
    Positional(usize),
}

impl SlotKey {
    pub(crate) fn from_slot(slot: &Slot) -> Self {
        match slot {
            Slot::Named(name) => SlotKey::Named(name.clone()),
            Slot::Positional(index) => SlotKey::Positional(*index),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    Call,
    From,
    Shortcut,
}

#[derive(Debug, Clone)]
pub(crate) struct Edge {
    pub from: DefId,
    pub to: DefId,
    pub kind: EdgeKind,
    pub args: Vec<ArgRef>,
}

#[derive(Debug)]
pub(crate) struct Declaration<'a> {
    pub id: DefId,
    pub def: &'a Definition,
    pub file: FileId,
    pub namespace: String,
    pub full_name: String,
    pub effective_id: String,
    pub template_depth: usize,
    pub file_scoped: bool,
    pub promoted: bool,
    pub refs: Vec<NodeRef>,
    pub read_slots: BTreeSet<SlotKey>,
    pub has_from: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NameRank {
    Full,
    Effective,
}

#[derive(Debug, Clone, Copy)]
struct NameCandidate {
    rank: NameRank,
    id: DefId,
}

#[derive(Debug, Clone, Copy)]
struct ResolveContext<'a> {
    file: FileId,
    namespace: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedRef {
    Component(DefId),
    Alias(String),
    Builtin,
    Unresolved,
}

#[derive(Debug, Clone)]
struct EntryInfo {
    file: FileId,
    namespace: String,
    qualified: String,
}

pub(crate) struct Graph<'a> {
    pub declarations: Vec<Declaration<'a>>,
    pub edges: Vec<Edge>,
    pub roots: BTreeSet<DefId>,
    pub used_aliases: BTreeSet<String>,
    pub entry_file: Option<FileId>,
    aliases: BTreeSet<String>,
    local_names: BTreeMap<(String, String), Vec<NameCandidate>>,
    file_names: BTreeMap<(u32, String), Vec<NameCandidate>>,
    qualified_names: BTreeMap<String, Vec<NameCandidate>>,
    promoted_names: BTreeMap<String, Vec<NameCandidate>>,
}

impl<'a> Graph<'a> {
    pub(crate) fn new(project: &'a Project, opts: &Options) -> Self {
        let declarations = collect_declarations(project, opts);
        let aliases: BTreeSet<String> = project.ipc.keys().cloned().collect();
        let mut local_names: BTreeMap<(String, String), Vec<NameCandidate>> = BTreeMap::new();
        let mut file_names: BTreeMap<(u32, String), Vec<NameCandidate>> = BTreeMap::new();
        let mut qualified_names: BTreeMap<String, Vec<NameCandidate>> = BTreeMap::new();
        let mut promoted_names: BTreeMap<String, Vec<NameCandidate>> = BTreeMap::new();

        for decl in &declarations {
            if decl.file_scoped {
                add_candidate(
                    &mut file_names,
                    (decl.file.0, decl.full_name.clone()),
                    NameRank::Full,
                    decl.id,
                );
                if decl.effective_id != decl.full_name {
                    add_candidate(
                        &mut file_names,
                        (decl.file.0, decl.effective_id.clone()),
                        NameRank::Effective,
                        decl.id,
                    );
                }
                continue;
            }

            add_candidate(
                &mut local_names,
                (decl.namespace.clone(), decl.full_name.clone()),
                NameRank::Full,
                decl.id,
            );
            if decl.effective_id != decl.full_name {
                add_candidate(
                    &mut local_names,
                    (decl.namespace.clone(), decl.effective_id.clone()),
                    NameRank::Effective,
                    decl.id,
                );
            }
            if !decl.namespace.is_empty() {
                add_candidate(
                    &mut qualified_names,
                    format!("{}.{}", decl.namespace, decl.full_name),
                    NameRank::Full,
                    decl.id,
                );
                if decl.effective_id != decl.full_name {
                    add_candidate(
                        &mut qualified_names,
                        format!("{}.{}", decl.namespace, decl.effective_id),
                        NameRank::Effective,
                        decl.id,
                    );
                }
            }
            if decl.promoted {
                add_candidate(
                    &mut promoted_names,
                    decl.full_name.clone(),
                    NameRank::Full,
                    decl.id,
                );
                if decl.effective_id != decl.full_name {
                    add_candidate(
                        &mut promoted_names,
                        decl.effective_id.clone(),
                        NameRank::Effective,
                        decl.id,
                    );
                }
            }
        }

        let mut graph = Graph {
            declarations,
            edges: Vec::new(),
            roots: BTreeSet::new(),
            used_aliases: BTreeSet::new(),
            entry_file: None,
            aliases,
            local_names,
            file_names,
            qualified_names,
            promoted_names,
        };

        let entry = resolve_entry(project, &opts.entry)
            .ok()
            .map(|(file, namespace, component)| {
                let qualified = if namespace.is_empty() {
                    component.to_string()
                } else {
                    format!("{namespace}.{component}")
                };
                EntryInfo {
                    file,
                    namespace,
                    qualified,
                }
            });
        graph.entry_file = entry.as_ref().map(|info| info.file);
        if let Some(info) = &entry {
            if let Some(id) = graph.resolve_decl(&info.qualified, info.file) {
                graph.roots.insert(id);
            }
        }

        graph.collect_edges(opts);
        graph.collect_test_roots(project, entry.as_ref());
        graph.collect_hook_roots(project, entry.as_ref());
        graph
    }

    pub(crate) fn template_predecessor(&self, id: DefId) -> Option<DefId> {
        let decl = &self.declarations[id];
        if decl.template_depth == 0 {
            return None;
        }
        let wanted_depth = decl.template_depth - 1;
        let mut best: Option<(u8, DefId)> = None;
        for predecessor in &self.declarations {
            if predecessor.template_depth != wanted_depth
                || predecessor.effective_id != decl.effective_id
            {
                continue;
            }
            let rank = if predecessor.file_scoped || decl.file_scoped {
                if predecessor.file_scoped && decl.file_scoped && predecessor.file == decl.file {
                    0
                } else {
                    continue;
                }
            } else if predecessor.namespace == decl.namespace {
                0
            } else if predecessor.namespace.is_empty() {
                1
            } else if predecessor.promoted {
                2
            } else {
                continue;
            };
            if best.is_none_or(|(best_rank, best_id)| (rank, predecessor.id) < (best_rank, best_id))
            {
                best = Some((rank, predecessor.id));
            }
        }
        best.map(|(_, id)| id)
    }

    pub(crate) fn implicit_template_edges(&self) -> Vec<(DefId, DefId)> {
        (0..self.declarations.len())
            .filter_map(|id| {
                self.template_predecessor(id)
                    .map(|predecessor| (predecessor, id))
            })
            .collect()
    }

    fn collect_edges(&mut self, opts: &Options) {
        let mut edges = Vec::new();
        let mut aliases = BTreeSet::new();
        for id in 0..self.declarations.len() {
            let context = ResolveContext {
                file: self.declarations[id].file,
                namespace: self.declarations[id].namespace.as_str(),
            };
            let refs = self.declarations[id].refs.clone();
            for reference in refs {
                match reference {
                    NodeRef::Call {
                        target,
                        span: _,
                        args,
                    } => match self.resolve_target(&target, &context) {
                        ResolvedRef::Component(to) => edges.push(Edge {
                            from: id,
                            to,
                            kind: EdgeKind::Call,
                            args,
                        }),
                        ResolvedRef::Alias(alias) => {
                            aliases.insert(alias);
                        }
                        ResolvedRef::Builtin | ResolvedRef::Unresolved => {}
                    },
                    NodeRef::From { target, span: _ } => {
                        match self.resolve_target(&target, &context) {
                            ResolvedRef::Component(to) => edges.push(Edge {
                                from: id,
                                to,
                                kind: EdgeKind::From,
                                args: Vec::new(),
                            }),
                            ResolvedRef::Alias(alias) => {
                                aliases.insert(alias);
                            }
                            ResolvedRef::Builtin | ResolvedRef::Unresolved => {}
                        }
                    }
                    NodeRef::PropertyKey { key, span: _ } => {
                        if is_shortcut_key(&key, &opts.from_keyword) {
                            match self.resolve_target(&key, &context) {
                                ResolvedRef::Component(to)
                                    if self.declarations[to].template_depth == 0 =>
                                {
                                    edges.push(Edge {
                                        from: id,
                                        to,
                                        kind: EdgeKind::Shortcut,
                                        args: Vec::new(),
                                    });
                                }
                                ResolvedRef::Alias(alias) => {
                                    aliases.insert(alias);
                                }
                                ResolvedRef::Component(_)
                                | ResolvedRef::Builtin
                                | ResolvedRef::Unresolved => {}
                            }
                        }
                    }
                    NodeRef::Interpolation { .. }
                    | NodeRef::SlotDecl { .. }
                    | NodeRef::ShellPayload { .. } => {}
                }
            }
        }
        self.edges = edges;
        self.used_aliases.extend(aliases);
    }

    fn collect_test_roots(&mut self, project: &Project, entry: Option<&EntryInfo>) {
        let Some(entry) = entry else {
            return;
        };
        let entry_target = (entry.file, entry.qualified.clone());
        let mut targets: Vec<(FileId, String)> = Vec::new();
        for (file, value) in &project.raw_meta_test {
            match value {
                Value::Object(fields)
                    if fields.contains_key("result") || fields.contains_key("error") =>
                {
                    targets.push(entry_target.clone());
                }
                Value::Object(fields) => {
                    for key in fields.keys() {
                        if !is_test_reserved_key(key) {
                            targets.push((*file, key.clone()));
                        }
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        if let Value::Object(fields) = item {
                            for key in fields.keys() {
                                if !is_test_reserved_key(key) {
                                    targets.push((*file, key.clone()));
                                }
                            }
                        }
                    }
                }
                Value::Null
                | Value::Bool(_)
                | Value::Int(_)
                | Value::Float(_)
                | Value::String(_) => {
                    targets.push(entry_target.clone());
                }
            }
        }
        for (file, target) in targets {
            if let Some(id) = self.resolve_same_file_decl(project, file, &target) {
                self.roots.insert(id);
            }
        }
    }

    fn collect_hook_roots(&mut self, project: &Project, entry: Option<&EntryInfo>) {
        let context = match entry {
            Some(info) => ResolveContext {
                file: info.file,
                namespace: info.namespace.as_str(),
            },
            None => ResolveContext {
                file: FileId(0),
                namespace: "",
            },
        };
        let mut hooks: Vec<String> = Vec::new();
        for spec in project.ipc.values() {
            for hook in [&spec.on_request, &spec.on_response, &spec.on_error]
                .into_iter()
                .flatten()
            {
                hooks.push(hook.clone());
            }
        }
        for hook in hooks {
            match self.resolve_target(&hook, &context) {
                ResolvedRef::Component(id) => {
                    self.roots.insert(id);
                }
                ResolvedRef::Alias(alias) => {
                    self.used_aliases.insert(alias);
                }
                ResolvedRef::Builtin | ResolvedRef::Unresolved => {}
            }
        }
    }

    fn resolve_same_file_decl(
        &self,
        project: &Project,
        file: FileId,
        target: &str,
    ) -> Option<DefId> {
        if target.contains('.') {
            if let Some(id) = best(self.qualified_names.get(target)) {
                if self.declarations[id].file == file {
                    return Some(id);
                }
            }
        }
        if let Some(id) = best(self.file_names.get(&(file.0, target.to_string()))) {
            return Some(id);
        }
        let mut candidates: Vec<(u8, DefId)> = Vec::new();
        for decl in &self.declarations {
            if decl.file != file {
                continue;
            }
            if decl.full_name == target {
                candidates.push((0, decl.id));
            } else if decl.effective_id == target {
                candidates.push((1, decl.id));
            }
        }
        if let Some((_, id)) = candidates.into_iter().min() {
            return Some(id);
        }
        let namespace = namespace_for_file(project, file);
        let qualified = if namespace.is_empty() {
            target.to_string()
        } else {
            format!("{namespace}.{target}")
        };
        self.resolve_decl(&qualified, file)
            .filter(|id| self.declarations[*id].file == file)
    }

    fn resolve_decl(&self, name: &str, file: FileId) -> Option<DefId> {
        if name.contains('.') {
            if let Some(id) = best(self.qualified_names.get(name)) {
                return Some(id);
            }
        }
        if let Some(id) = best(self.file_names.get(&(file.0, name.to_string()))) {
            return Some(id);
        }
        if let Some(id) = best(self.local_names.get(&(String::new(), name.to_string()))) {
            return Some(id);
        }
        best(self.promoted_names.get(name))
    }

    fn resolve_target(&self, target: &str, context: &ResolveContext) -> ResolvedRef {
        let effective = strip_dollars(target);
        if target.contains('.') {
            if let Some(id) = best(self.qualified_names.get(target)) {
                return ResolvedRef::Component(id);
            }
        }
        if is_builtin_effective(&effective) {
            return ResolvedRef::Builtin;
        }
        if let Some(alias) = self.match_alias(target, &effective) {
            return ResolvedRef::Alias(alias);
        }
        if let Some(id) = best(self.file_names.get(&(context.file.0, target.to_string()))) {
            return ResolvedRef::Component(id);
        }
        if effective.starts_with('_') {
            return ResolvedRef::Unresolved;
        }
        if let Some(id) = best(
            self.local_names
                .get(&(context.namespace.to_string(), target.to_string())),
        ) {
            return ResolvedRef::Component(id);
        }
        if !context.namespace.is_empty() {
            if let Some(id) = best(self.local_names.get(&(String::new(), target.to_string()))) {
                return ResolvedRef::Component(id);
            }
        }
        match best(self.promoted_names.get(target)) {
            Some(id) => ResolvedRef::Component(id),
            None => ResolvedRef::Unresolved,
        }
    }

    fn match_alias(&self, target: &str, effective: &str) -> Option<String> {
        if self.aliases.contains(target) {
            return Some(target.to_string());
        }
        if self.aliases.contains(effective) {
            return Some(effective.to_string());
        }
        self.aliases
            .iter()
            .find(|alias| strip_dollars(alias) == effective)
            .cloned()
    }
}

fn collect_declarations<'a>(project: &'a Project, opts: &Options) -> Vec<Declaration<'a>> {
    let mut declarations: Vec<Declaration<'a>> = Vec::new();
    let mut paths: Vec<&str> = project
        .namespaces
        .namespaces()
        .map(|(path, _)| path)
        .collect();
    paths.sort_unstable();
    for path in paths {
        let namespace = project
            .namespaces
            .namespace(path)
            .expect("path came from namespaces()");
        let mut defs: Vec<(&str, &Definition)> = namespace.defs().collect();
        defs.sort_unstable_by_key(|(name, _)| *name);
        for (name, def) in defs {
            if is_builtin_def(name) {
                continue;
            }
            declarations.push(make_declaration(declarations.len(), def, path, opts));
        }
    }

    let mut file_defs: Vec<(FileId, &str, &Definition)> = project.file_scoped.defs().collect();
    file_defs.sort_unstable_by(|a, b| {
        file_sort_key(project, a.0)
            .cmp(&file_sort_key(project, b.0))
            .then_with(|| a.1.cmp(b.1))
    });
    for (file, name, def) in file_defs {
        if is_builtin_def(name) {
            continue;
        }
        let namespace = namespace_for_file(project, file);
        declarations.push(make_declaration(declarations.len(), def, &namespace, opts));
    }

    declarations.sort_by(|a, b| {
        file_sort_key(project, a.file)
            .cmp(&file_sort_key(project, b.file))
            .then_with(|| a.def.span.line.cmp(&b.def.span.line))
            .then_with(|| a.def.span.col.cmp(&b.def.span.col))
            .then_with(|| a.namespace.cmp(&b.namespace))
            .then_with(|| a.full_name.cmp(&b.full_name))
    });
    for (id, decl) in declarations.iter_mut().enumerate() {
        decl.id = id;
        decl.promoted = is_promoted(decl, &opts.plain);
    }
    declarations
}

fn make_declaration<'a>(
    id: DefId,
    def: &'a Definition,
    namespace: &str,
    opts: &Options,
) -> Declaration<'a> {
    let (template_depth, effective_id, file_scoped) = decompose(&def.full_name);
    let refs = refs::collect_node_refs(&def.body, &opts.from_keyword);
    let mut read_slots = BTreeSet::new();
    let mut has_from = false;
    for reference in &refs {
        match reference {
            NodeRef::Interpolation { slot, .. } => {
                read_slots.insert(SlotKey::from_slot(slot));
            }
            NodeRef::From { .. } => has_from = true,
            _ => {}
        }
    }
    Declaration {
        id,
        def,
        file: def.file,
        namespace: namespace.to_string(),
        full_name: def.full_name.clone(),
        effective_id,
        template_depth,
        file_scoped,
        promoted: false,
        refs,
        read_slots,
        has_from,
    }
}

fn add_candidate<K: Ord>(
    map: &mut BTreeMap<K, Vec<NameCandidate>>,
    key: K,
    rank: NameRank,
    id: DefId,
) {
    map.entry(key).or_default().push(NameCandidate { rank, id });
}

fn best(candidates: Option<&Vec<NameCandidate>>) -> Option<DefId> {
    candidates
        .and_then(|entries| {
            entries
                .iter()
                .min_by_key(|candidate| (candidate.rank, candidate.id))
        })
        .map(|candidate| candidate.id)
}

fn decompose(full_name: &str) -> (usize, String, bool) {
    match classify(full_name, Span::default()) {
        DefClass::Component(meta)
        | DefClass::BuiltinReserved(meta)
        | DefClass::MetaReserved(meta) => (
            meta.dollar_count as usize,
            meta.effective_id,
            meta.file_scoped,
        ),
        DefClass::InvalidName(_) | DefClass::MetaBare(..) => {
            let template_depth = full_name.chars().take_while(|ch| *ch == '$').count();
            let effective = strip_dollars(full_name);
            let file_scoped = effective.starts_with('_');
            (template_depth, effective, file_scoped)
        }
    }
}

fn is_builtin_def(full_name: &str) -> bool {
    let (_, effective, _) = decompose(full_name);
    is_builtin_effective(&effective)
}

pub(crate) fn is_builtin_effective(effective: &str) -> bool {
    effective == "sh" || effective == "pw" || Builtin::is_reserved(effective)
}

pub(crate) fn strip_dollars(name: &str) -> String {
    name.trim_start_matches('$').to_string()
}

fn is_shortcut_key(key: &str, from_keyword: &str) -> bool {
    key != from_keyword && !key.contains('$') && !key.contains('?') && key.parse::<i64>().is_err()
}

fn is_test_reserved_key(key: &str) -> bool {
    matches!(key, "result" | "error" | "args" | "_build_error")
}

fn is_promoted(decl: &Declaration<'_>, plain: &PlainMode) -> bool {
    if decl.file_scoped
        || decl.namespace.is_empty()
        || !matches!(plain, PlainMode::All | PlainMode::TemplatesOnly)
    {
        return false;
    }
    !matches!(plain, PlainMode::TemplatesOnly) || decl.template_depth > 0
}

fn file_sort_key(project: &Project, file: FileId) -> String {
    project
        .files
        .get(file.0 as usize)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<?>".to_string())
}

fn namespace_for_file(project: &Project, file: FileId) -> String {
    let Some(path) = project.files.get(file.0 as usize) else {
        return String::new();
    };
    let relative = path.strip_prefix(&project.root).unwrap_or(path);
    let Some(parent) = relative.parent() else {
        return String::new();
    };
    parent
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(".")
}
