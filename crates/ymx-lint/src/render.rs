use serde_json::{json, Map, Value};

use crate::{FailOn, LintConfig, LintDiagnostic, LintReport, LintSeverity};

pub fn render_diagnostics(report: &LintReport) -> String {
    let mut lines = Vec::new();
    for finding in &report.findings {
        lines.push(format!(
            "[{}] {}: {}:{}:{} ({}): {}",
            finding.code,
            severity_name(finding.severity),
            file_name(&finding.file),
            finding.line,
            finding.col,
            component_name(&finding.component),
            finding.message
        ));
        if let Some(help) = &finding.help {
            lines.push(format!("  help: {help}"));
        }
    }
    for diagnostic in &report.diagnostics {
        lines.push(format!(
            "[compile {}] {}:{}:{} ({}): {}",
            diagnostic.code,
            file_name(&diagnostic.file),
            diagnostic.line,
            diagnostic.col,
            component_name(&diagnostic.component),
            diagnostic.message
        ));
    }
    lines.join("\n")
}

pub fn render_json(report: &LintReport) -> Result<String, serde_json::Error> {
    let findings: Vec<Value> = report.findings.iter().map(finding_json).collect();
    let diagnostics: Vec<Value> = report.diagnostics.iter().map(diagnostic_json).collect();
    let errors = report
        .findings
        .iter()
        .filter(|finding| finding.severity == LintSeverity::Error)
        .count();
    let warnings = report
        .findings
        .iter()
        .filter(|finding| finding.severity == LintSeverity::Warning)
        .count();
    let info = report
        .findings
        .iter()
        .filter(|finding| finding.severity == LintSeverity::Info)
        .count();

    let mut root = Map::new();
    root.insert("findings".to_string(), Value::Array(findings));
    root.insert("diagnostics".to_string(), Value::Array(diagnostics));
    root.insert(
        "summary".to_string(),
        json!({
            "findings": report.findings.len(),
            "errors": errors,
            "warnings": warnings,
            "info": info,
        }),
    );
    serde_json::to_string(&Value::Object(root))
}

pub fn should_fail(report: &LintReport, config: &LintConfig) -> bool {
    let threshold = match config.fail_on {
        FailOn::None => return false,
        FailOn::Error => 3,
        FailOn::Warning => 2,
        FailOn::Info => 1,
    };
    report
        .findings
        .iter()
        .any(|finding| severity_rank(finding.severity) >= threshold)
}

fn severity_name(severity: LintSeverity) -> &'static str {
    match severity {
        LintSeverity::Error => "error",
        LintSeverity::Warning => "warning",
        LintSeverity::Info => "info",
    }
}

fn severity_rank(severity: LintSeverity) -> u8 {
    match severity {
        LintSeverity::Error => 3,
        LintSeverity::Warning => 2,
        LintSeverity::Info => 1,
    }
}

fn file_name(file: &Option<std::path::PathBuf>) -> String {
    file.as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<?>".to_string())
}

fn component_name(component: &Option<String>) -> String {
    component.clone().unwrap_or_else(|| "<?>".to_string())
}

fn finding_json(finding: &LintDiagnostic) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), json!(finding.code));
    object.insert(
        "severity".to_string(),
        json!(severity_name(finding.severity)),
    );
    object.insert(
        "file".to_string(),
        json!(finding.file.as_ref().map(|path| path.display().to_string())),
    );
    object.insert("line".to_string(), json!(finding.line));
    object.insert("col".to_string(), json!(finding.col));
    object.insert("component".to_string(), json!(finding.component));
    object.insert("message".to_string(), json!(finding.message));
    object.insert("help".to_string(), json!(finding.help));
    Value::Object(object)
}

fn diagnostic_json(diagnostic: &ymx_core::diag::Diagnostic) -> Value {
    let mut object = Map::new();
    object.insert("code".to_string(), json!(diagnostic.code));
    object.insert(
        "file".to_string(),
        json!(diagnostic
            .file
            .as_ref()
            .map(|path| path.display().to_string())),
    );
    object.insert("line".to_string(), json!(diagnostic.line));
    object.insert("col".to_string(), json!(diagnostic.col));
    object.insert("component".to_string(), json!(diagnostic.component));
    object.insert("message".to_string(), json!(diagnostic.message));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ymx_core::diag::Diagnostic;

    use super::*;

    fn finding() -> LintDiagnostic {
        LintDiagnostic {
            code: crate::L001,
            severity: LintSeverity::Warning,
            file: Some(PathBuf::from("main.yml")),
            line: 3,
            col: 1,
            component: Some("unused".to_string()),
            message: "component is never referenced".to_string(),
            help: Some("remove the definition or add a call site".to_string()),
        }
    }

    fn report() -> LintReport {
        LintReport {
            findings: vec![finding()],
            diagnostics: vec![Diagnostic {
                file: Some(PathBuf::from("main.yml")),
                line: 7,
                col: 5,
                component: Some("main".to_string()),
                code: "E002",
                message: "unknown component reference".to_string(),
            }],
        }
    }

    #[test]
    fn diagnostics_render_findings_then_compiler_diagnostics() {
        let expected = "[L001] warning: main.yml:3:1 (unused): component is never referenced\n  help: remove the definition or add a call site\n[compile E002] main.yml:7:5 (main): unknown component reference";
        assert_eq!(render_diagnostics(&report()), expected);
    }

    #[test]
    fn diagnostics_render_placeholders_and_omit_missing_help() {
        let mut finding = finding();
        finding.file = None;
        finding.component = None;
        finding.help = None;
        let report = LintReport {
            findings: vec![finding],
            diagnostics: Vec::new(),
        };
        assert_eq!(
            render_diagnostics(&report),
            "[L001] warning: <?>:3:1 (<?>): component is never referenced"
        );
    }

    #[test]
    fn json_render_has_separate_arrays_and_lint_only_summary() {
        let expected = "{\"findings\":[{\"code\":\"L001\",\"severity\":\"warning\",\"file\":\"main.yml\",\"line\":3,\"col\":1,\"component\":\"unused\",\"message\":\"component is never referenced\",\"help\":\"remove the definition or add a call site\"}],\"diagnostics\":[{\"code\":\"E002\",\"file\":\"main.yml\",\"line\":7,\"col\":5,\"component\":\"main\",\"message\":\"unknown component reference\"}],\"summary\":{\"findings\":1,\"errors\":0,\"warnings\":1,\"info\":0}}";
        assert_eq!(render_json(&report()).unwrap(), expected);
    }

    #[test]
    fn json_render_uses_null_for_missing_file_component_and_help() {
        let mut finding = finding();
        finding.file = None;
        finding.component = None;
        finding.help = None;
        finding.severity = LintSeverity::Info;
        let report = LintReport {
            findings: vec![finding],
            diagnostics: vec![Diagnostic {
                file: None,
                line: 1,
                col: 1,
                component: None,
                code: "E018",
                message: "ipc disabled".to_string(),
            }],
        };
        let value: Value = serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        assert_eq!(value["findings"][0]["file"], Value::Null);
        assert_eq!(value["findings"][0]["component"], Value::Null);
        assert_eq!(value["findings"][0]["help"], Value::Null);
        assert_eq!(value["findings"][0]["severity"], json!("info"));
        assert_eq!(value["diagnostics"][0]["file"], Value::Null);
        assert_eq!(value["summary"]["info"], json!(1));
    }

    #[test]
    fn severity_application_prefers_allow_over_deny() {
        let mut findings = vec![finding()];
        let config = LintConfig {
            deny: vec![crate::L001],
            allow: vec![crate::L001],
            ..LintConfig::default()
        };
        crate::rules::apply_config(&mut findings, &config);
        assert!(findings.is_empty());
    }

    #[test]
    fn severity_application_denies_and_defaults_to_warning() {
        let mut findings = vec![finding(), {
            let mut other = finding();
            other.code = crate::L003;
            other
        }];
        let config = LintConfig {
            deny: vec![crate::L001],
            allow: Vec::new(),
            ..LintConfig::default()
        };
        crate::rules::apply_config(&mut findings, &config);
        assert_eq!(findings[0].severity, LintSeverity::Error);
        assert_eq!(findings[1].severity, LintSeverity::Warning);
    }

    #[test]
    fn should_fail_honors_all_thresholds() {
        let report = LintReport {
            findings: vec![finding()],
            diagnostics: Vec::new(),
        };
        assert!(!should_fail(
            &report,
            &LintConfig {
                fail_on: FailOn::Error,
                ..LintConfig::default()
            }
        ));
        assert!(should_fail(
            &report,
            &LintConfig {
                fail_on: FailOn::Warning,
                ..LintConfig::default()
            }
        ));
        assert!(should_fail(
            &report,
            &LintConfig {
                fail_on: FailOn::Info,
                ..LintConfig::default()
            }
        ));
        assert!(!should_fail(
            &report,
            &LintConfig {
                fail_on: FailOn::None,
                ..LintConfig::default()
            }
        ));
    }
}
