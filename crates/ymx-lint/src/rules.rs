use std::collections::BTreeSet;

use ymx_core::diag::Span;
use ymx_core::project::Project;
use ymx_core::refs::{ArgRef, NodeRef, Slot};

use crate::graph::{strip_dollars, Declaration, DefId, EdgeKind, Graph, SlotKey};
use crate::{LintConfig, LintDiagnostic, LintSeverity, L001, L002, L003, L004, L005, L006};

pub(crate) fn run(graph: &Graph<'_>, project: &Project) -> Vec<LintDiagnostic> {
    let mut findings = Vec::new();
    unused_definitions(graph, project, &mut findings);
    unreachable_definitions(graph, project, &mut findings);
    unused_arguments(graph, project, &mut findings);
    unused_called_arguments(graph, project, &mut findings);
    interpolated_shell_payloads(graph, project, &mut findings);
    unused_ipc_aliases(graph, project, &mut findings);
    findings
}

pub(crate) fn apply_config(findings: &mut Vec<LintDiagnostic>, config: &LintConfig) {
    findings.retain(|finding| !config.allow.contains(&finding.code));
    for finding in findings.iter_mut() {
        finding.severity = if config.deny.contains(&finding.code) {
            LintSeverity::Error
        } else {
            LintSeverity::Warning
        };
    }
}

fn unused_definitions(graph: &Graph<'_>, project: &Project, findings: &mut Vec<LintDiagnostic>) {
    let incoming: BTreeSet<DefId> = graph.edges.iter().map(|edge| edge.to).collect();
    let mut used: Vec<bool> = (0..graph.declarations.len())
        .map(|id| graph.roots.contains(&id) || incoming.contains(&id))
        .collect();
    let max_depth = graph
        .declarations
        .iter()
        .map(|decl| decl.template_depth)
        .max()
        .unwrap_or(0);
    for depth in 1..=max_depth {
        for id in 0..graph.declarations.len() {
            if used[id] || graph.declarations[id].template_depth != depth {
                continue;
            }
            if let Some(predecessor) = graph.template_predecessor(id) {
                if used[predecessor] {
                    used[id] = true;
                }
            }
        }
    }
    for (id, decl) in graph.declarations.iter().enumerate() {
        if !used[id] {
            findings.push(decl_finding(
                project,
                decl,
                L001,
                decl.def.span,
                format!("component `{}` is never referenced", decl.full_name),
                "remove the definition, or add a call site.",
            ));
        }
    }
}

fn unreachable_definitions(
    graph: &Graph<'_>,
    project: &Project,
    findings: &mut Vec<LintDiagnostic>,
) {
    let implicit = graph.implicit_template_edges();
    let mut reachable = graph.roots.clone();
    let mut stack: Vec<DefId> = reachable.iter().copied().collect();
    while let Some(id) = stack.pop() {
        for edge in graph.edges.iter().filter(|edge| edge.from == id) {
            if reachable.insert(edge.to) {
                stack.push(edge.to);
            }
        }
        for (from, to) in &implicit {
            if *from == id && reachable.insert(*to) {
                stack.push(*to);
            }
        }
    }
    for (id, decl) in graph.declarations.iter().enumerate() {
        if decl.template_depth == 0 && !reachable.contains(&id) {
            findings.push(decl_finding(
                project,
                decl,
                L002,
                decl.def.span,
                format!("component `{}` is unreachable from the entry", decl.full_name),
                "wire this component into a `from`, `$name(...)`, `$name{...}`, shortcut, or math call reachable from the entry.",
            ));
        }
    }
}

fn unused_arguments(graph: &Graph<'_>, project: &Project, findings: &mut Vec<LintDiagnostic>) {
    for decl in &graph.declarations {
        if decl.has_from {
            continue;
        }
        for reference in &decl.refs {
            if let NodeRef::SlotDecl { slot, span } = reference {
                if !decl.read_slots.contains(&SlotKey::from_slot(slot)) {
                    findings.push(decl_finding(
                        project,
                        decl,
                        L003,
                        *span,
                        format!(
                            "argument `{}` in `{}` is never read",
                            slot_label(slot),
                            decl.full_name
                        ),
                        "remove the unused property, or reference it with `$name`.",
                    ));
                }
            }
        }
    }
}

fn unused_called_arguments(
    graph: &Graph<'_>,
    project: &Project,
    findings: &mut Vec<LintDiagnostic>,
) {
    for edge in &graph.edges {
        if edge.kind != EdgeKind::Call || edge.args.is_empty() {
            continue;
        }
        let target = &graph.declarations[edge.to];
        if target.template_depth > 0 || target.has_from {
            continue;
        }
        let caller = &graph.declarations[edge.from];
        for arg in &edge.args {
            if !target.read_slots.contains(&SlotKey::from_slot(&arg.slot)) {
                findings.push(decl_finding(
                    project,
                    caller,
                    L004,
                    arg.span,
                    format!(
                        "`{}` passes unused argument `{}` to `{}`",
                        caller.full_name,
                        slot_label(&arg.slot),
                        target.full_name
                    ),
                    "drop the unused argument `name=value`.",
                ));
            }
        }
    }
}

fn interpolated_shell_payloads(
    graph: &Graph<'_>,
    project: &Project,
    findings: &mut Vec<LintDiagnostic>,
) {
    for decl in &graph.declarations {
        for reference in &decl.refs {
            match reference {
                NodeRef::ShellPayload {
                    interpolated: true,
                    span,
                    ..
                } => findings.push(decl_finding(
                    project,
                    decl,
                    L005,
                    *span,
                    format!("shell payload in `{}` contains interpolation", decl.full_name),
                    "pass data as separate arguments, or escape/sanitize the interpolated value before it reaches the shell.",
                )),
                NodeRef::Call {
                    target,
                    args,
                    span: _,
                } => {
                    let effective = strip_dollars(target);
                    if effective == "sh" || effective == "pw" {
                        for arg_span in interpolated_arg_spans(&decl.refs, args) {
                            findings.push(decl_finding(
                                project,
                                decl,
                                L005,
                                arg_span,
                                format!(
                                    "shell payload in `{}` contains interpolation",
                                    decl.full_name
                                ),
                                "pass data as separate arguments, or escape/sanitize the interpolated value before it reaches the shell.",
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn interpolated_arg_spans(refs: &[NodeRef], args: &[ArgRef]) -> Vec<Span> {
    let reads: Vec<(u32, u32)> = refs
        .iter()
        .filter_map(|reference| match reference {
            NodeRef::Interpolation { span, .. } => Some((span.line, span.col)),
            _ => None,
        })
        .collect();
    args.iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            let start = (arg.span.line, arg.span.col);
            let end = args
                .get(index + 1)
                .map(|next| (next.span.line, next.span.col));
            let contains = reads
                .iter()
                .any(|read| *read >= start && end.is_none_or(|upper| *read < upper));
            if contains {
                Some(arg.span)
            } else {
                None
            }
        })
        .collect()
}

fn unused_ipc_aliases(graph: &Graph<'_>, project: &Project, findings: &mut Vec<LintDiagnostic>) {
    for alias in project.ipc.keys() {
        if graph.used_aliases.contains(alias) {
            continue;
        }
        findings.push(LintDiagnostic {
            code: L006,
            severity: LintSeverity::Warning,
            file: graph
                .entry_file
                .and_then(|file| project.files.get(file.0 as usize).cloned()),
            line: 1,
            col: 1,
            component: Some(alias.clone()),
            message: format!("IPC alias `{alias}` is never called"),
            help: Some("remove the `_use` entry, or add a call site.".to_string()),
        });
    }
}

fn decl_finding(
    project: &Project,
    decl: &Declaration<'_>,
    code: &'static str,
    span: Span,
    message: String,
    help: &str,
) -> LintDiagnostic {
    LintDiagnostic {
        code,
        severity: LintSeverity::Warning,
        file: project.files.get(decl.file.0 as usize).cloned(),
        line: span.line,
        col: span.col,
        component: Some(decl.full_name.clone()),
        message,
        help: Some(help.to_string()),
    }
}

fn slot_label(slot: &Slot) -> String {
    match slot {
        Slot::Named(name) => name.clone(),
        Slot::Positional(index) => format!("${index}"),
    }
}
