pub use ymx_config::{
    FailOn, LintConfig, LintOverrides, LintSeverity, L001, L002, L003, L004, L005, L006,
};

use std::path::PathBuf;

use ymx_core::diag::{Diagnostic, E016, E018};
use ymx_core::project::{Options, Project};
use ymx_core::resolve::compile;

mod graph;
mod render;
mod rules;

pub use render::{render_diagnostics, render_json, should_fail};

#[derive(Debug, Clone, PartialEq)]
pub struct LintDiagnostic {
    pub code: &'static str,
    pub severity: LintSeverity,
    pub file: Option<PathBuf>,
    pub line: u32,
    pub col: u32,
    pub component: Option<String>,
    pub message: String,
    pub help: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LintReport {
    pub findings: Vec<LintDiagnostic>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lint_project(project: &Project, opts: &Options, config: &LintConfig) -> LintReport {
    let mut static_opts = opts.clone();
    static_opts.executor = None;
    static_opts.ipc = None;

    let mut diagnostics = match compile(project, &static_opts) {
        Ok(_) => Vec::new(),
        Err(diags) => filter_static_diagnostics(diags),
    };
    diagnostics.sort_by(|a, b| diagnostic_sort_key(a).cmp(&diagnostic_sort_key(b)));

    let graph = graph::Graph::new(project, opts);
    let mut findings = rules::run(&graph, project);
    rules::apply_config(&mut findings, config);
    findings.sort_by(|a, b| finding_sort_key(a).cmp(&finding_sort_key(b)));
    findings.dedup_by(|a, b| {
        a.code == b.code
            && a.severity == b.severity
            && a.file == b.file
            && a.line == b.line
            && a.col == b.col
            && a.component == b.component
            && a.message == b.message
            && a.help == b.help
    });

    LintReport {
        findings,
        diagnostics,
    }
}

pub(crate) fn filter_static_diagnostics(diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diags
        .into_iter()
        .filter(|diag| diag.code != E016 && diag.code != E018)
        .collect()
}

fn finding_sort_key(finding: &LintDiagnostic) -> (String, u32, u32, String, &'static str, String) {
    (
        finding
            .file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        finding.line,
        finding.col,
        finding.component.clone().unwrap_or_default(),
        finding.code,
        finding.message.clone(),
    )
}

fn diagnostic_sort_key(diag: &Diagnostic) -> (String, u32, u32, String, &'static str, String) {
    (
        diag.file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        diag.line,
        diag.col,
        diag.component.clone().unwrap_or_default(),
        diag.code,
        diag.message.clone(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ymx_core::diag::{FileId, Span};
    use ymx_core::namespace::Definition;
    use ymx_core::parse::parse_document;

    use super::*;

    fn diag(code: &'static str) -> Diagnostic {
        Diagnostic {
            file: None,
            line: 1,
            col: 1,
            component: None,
            code,
            message: code.to_string(),
        }
    }

    #[test]
    fn static_filter_removes_only_execution_diags() {
        let diags = filter_static_diagnostics(vec![
            diag("E002"),
            diag(E016),
            diag("E003"),
            diag(E018),
            diag("E011"),
        ]);
        let codes: Vec<&str> = diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, ["E002", "E003", "E011"]);
    }

    fn def(name: &str, body: &str) -> Definition {
        Definition {
            file: FileId(0),
            full_name: name.to_string(),
            span: Span { line: 1, col: 1 },
            body: parse_document(body).expect("inline yaml"),
            math_shorthand: false,
            trailing_question: false,
            exec_backend: None,
        }
    }

    fn inline_project() -> Project {
        let mut project = Project::new();
        project.root = PathBuf::from("/proj");
        project.files = vec![PathBuf::from("/proj/main.yml")];
        project
            .namespaces
            .register("", def("main", "x: 1\nunused: 2\nvalue: $x $value\n"))
            .unwrap();
        project
            .namespaces
            .register("", def("orphan", "{}\n"))
            .unwrap();
        project
    }

    #[test]
    fn graph_rules_report_unused_definition_and_argument() {
        let project = inline_project();
        let report = lint_project(&project, &Options::default(), &LintConfig::default());
        let tuples: Vec<(&str, Option<&str>)> = report
            .findings
            .iter()
            .map(|finding| (finding.code, finding.component.as_deref()))
            .collect();
        assert!(tuples.contains(&(L001, Some("orphan"))));
        assert!(tuples.contains(&(L003, Some("main"))));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == L003 && finding.message.contains("unused")));
    }

    #[test]
    fn lint_project_is_byte_identical_for_identical_inputs() {
        let project = inline_project();
        let options = Options::default();
        let config = LintConfig::default();
        let first = lint_project(&project, &options, &config);
        let second = lint_project(&project, &options, &config);
        assert_eq!(render_diagnostics(&first), render_diagnostics(&second));
        assert_eq!(render_json(&first).unwrap(), render_json(&second).unwrap());
    }
}
