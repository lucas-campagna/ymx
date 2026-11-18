use std::path::{Path, PathBuf};
use std::sync::Arc;

use ymx_config::{extract_lint_config, extract_options, CliOverrides, FailOn, LintOverrides};
use ymx_core::exec::CommandExecutor;
use ymx_lib::{StdExecutor, StdIpcHost};
use ymx_lint::{
    lint_project, render_diagnostics, render_json, should_fail, LintReport, LintSeverity, L001,
    L002, L003, L004, L005, L006,
};

type Tuple = (&'static str, LintSeverity, String, u32, u32, String);

struct Fixture {
    root: PathBuf,
    report: LintReport,
    config: ymx_lint::LintConfig,
}

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lint")
        .join(relative)
}

fn load(relative: &str, lint_overrides: &LintOverrides) -> Fixture {
    let root = fixture_path(relative);
    let project =
        ymx_lib::load_project(&root).unwrap_or_else(|diags| panic!("load failed: {diags:#?}"));
    let options_overrides = CliOverrides {
        entry: Some("main.main".to_string()),
        ..CliOverrides::default_for_tests()
    };
    let opts = extract_options(&project, &options_overrides)
        .unwrap_or_else(|diags| panic!("options failed: {diags:#?}"));
    let config = extract_lint_config(&project, lint_overrides)
        .unwrap_or_else(|diags| panic!("lint config failed: {diags:#?}"));
    let report = lint_project(&project, &opts, &config);
    Fixture {
        root,
        report,
        config,
    }
}

fn tuples(fixture: &Fixture) -> Vec<Tuple> {
    fixture
        .report
        .findings
        .iter()
        .map(|finding| {
            let file = finding.file.as_deref().map_or_else(
                || "<?>".to_string(),
                |path| {
                    path.strip_prefix(&fixture.root)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                },
            );
            (
                finding.code,
                finding.severity,
                file,
                finding.line,
                finding.col,
                finding.component.clone().unwrap_or_default(),
            )
        })
        .collect()
}

fn diagnostic_codes(fixture: &Fixture) -> Vec<&str> {
    fixture
        .report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

fn warning(file: &str, line: u32, col: u32, component: &str) -> Tuple {
    (
        "",
        LintSeverity::Warning,
        file.to_string(),
        line,
        col,
        component.to_string(),
    )
}

fn with_code(mut tuple: Tuple, code: &'static str) -> Tuple {
    tuple.0 = code;
    tuple
}

#[test]
fn l001_positive_reports_unused_component() {
    let fixture = load("rules/L001/positive", &LintOverrides::default_for_tests());
    assert_eq!(
        tuples(&fixture),
        vec![
            with_code(warning("main.yml", 2, 1, "unused"), L001),
            with_code(warning("main.yml", 2, 1, "unused"), L002),
        ]
    );
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l001_negative_has_no_findings() {
    let fixture = load("rules/L001/negative", &LintOverrides::default_for_tests());
    assert!(tuples(&fixture).is_empty());
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l002_positive_distinguishes_unused_from_unreachable() {
    let fixture = load("rules/L002/positive", &LintOverrides::default_for_tests());
    assert_eq!(
        tuples(&fixture),
        vec![
            with_code(warning("main.yml", 2, 1, "orphan"), L001),
            with_code(warning("main.yml", 2, 1, "orphan"), L002),
            with_code(warning("main.yml", 3, 1, "detached"), L002),
        ]
    );
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l002_negative_has_no_findings() {
    let fixture = load("rules/L002/negative", &LintOverrides::default_for_tests());
    assert!(tuples(&fixture).is_empty());
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l003_positive_reports_unused_argument() {
    let fixture = load("rules/L003/positive", &LintOverrides::default_for_tests());
    assert_eq!(
        tuples(&fixture),
        vec![with_code(warning("main.yml", 2, 3, "main"), L003)]
    );
    assert_eq!(diagnostic_codes(&fixture), vec!["E003"]);
}

#[test]
fn l003_negative_has_no_findings() {
    let fixture = load("rules/L003/negative", &LintOverrides::default_for_tests());
    assert!(tuples(&fixture).is_empty());
    assert_eq!(diagnostic_codes(&fixture), vec!["E003"]);
}

#[test]
fn l004_positive_reports_unused_passed_argument_at_call_span() {
    let fixture = load("rules/L004/positive", &LintOverrides::default_for_tests());
    assert_eq!(
        tuples(&fixture),
        vec![with_code(warning("main.yml", 1, 22, "main"), L004)]
    );
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l004_negative_has_no_findings() {
    let fixture = load("rules/L004/negative", &LintOverrides::default_for_tests());
    assert!(tuples(&fixture).is_empty());
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l005_positive_reports_interpolated_shell_payload() {
    let fixture = load("rules/L005/positive", &LintOverrides::default_for_tests());
    assert_eq!(
        tuples(&fixture),
        vec![with_code(warning("main.yml", 3, 9, "main"), L005)]
    );
    assert_eq!(diagnostic_codes(&fixture), vec!["E003"]);
}

#[test]
fn l005_negative_has_no_findings() {
    let fixture = load("rules/L005/negative", &LintOverrides::default_for_tests());
    assert!(tuples(&fixture).is_empty());
    assert_eq!(diagnostic_codes(&fixture), vec!["E003"]);
}

#[test]
fn l006_positive_reports_unused_ipc_alias() {
    let fixture = load("rules/L006/positive", &LintOverrides::default_for_tests());
    assert_eq!(
        tuples(&fixture),
        vec![with_code(warning("main.yml", 1, 1, "echo"), L006)]
    );
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn l006_negative_has_no_findings() {
    let fixture = load("rules/L006/negative", &LintOverrides::default_for_tests());
    assert!(tuples(&fixture).is_empty());
    assert!(diagnostic_codes(&fixture).is_empty());
}

#[test]
fn config_precedence_from_entry_file_and_cli_overrides() {
    let fixture = load("config/precedence", &LintOverrides::default_for_tests());
    assert_eq!(fixture.config.deny, vec![L001]);
    assert_eq!(fixture.config.fail_on, FailOn::Error);
    assert_eq!(
        tuples(&fixture),
        vec![
            (
                L001,
                LintSeverity::Error,
                "main.yml".to_string(),
                6,
                1,
                "unused".to_string()
            ),
            with_code(warning("main.yml", 6, 1, "unused"), L002),
        ]
    );
    assert!(should_fail(&fixture.report, &fixture.config));

    let allow = LintOverrides {
        allow: Some(vec!["L002".to_string()]),
        ..LintOverrides::default_for_tests()
    };
    let allowed = load("config/precedence", &allow);
    assert_eq!(
        tuples(&allowed),
        vec![(
            L001,
            LintSeverity::Error,
            "main.yml".to_string(),
            6,
            1,
            "unused".to_string()
        )]
    );
    assert!(should_fail(&allowed.report, &allowed.config));

    let cli_allows_l001 = LintOverrides {
        allow: Some(vec!["L001".to_string()]),
        ..LintOverrides::default_for_tests()
    };
    let l001_allowed = load("rules/L001/positive", &cli_allows_l001);
    assert_eq!(
        tuples(&l001_allowed),
        vec![with_code(warning("main.yml", 2, 1, "unused"), L002)]
    );

    let none = LintOverrides {
        fail_on: Some(FailOn::None),
        ..LintOverrides::default_for_tests()
    };
    let no_fail = load("config/precedence", &none);
    assert_eq!(no_fail.config.fail_on, FailOn::None);
    assert_eq!(
        tuples(&no_fail),
        vec![
            (
                L001,
                LintSeverity::Error,
                "main.yml".to_string(),
                6,
                1,
                "unused".to_string()
            ),
            with_code(warning("main.yml", 6, 1, "unused"), L002),
        ]
    );
    assert!(!should_fail(&no_fail.report, &no_fail.config));
}

#[test]
fn malformed_non_entry_lint_config_is_ignored() {
    let root = fixture_path("config/precedence");
    let project = ymx_lib::load_project(&root).expect("non-entry _ymx must not break loading");
    let lint_overrides = LintOverrides::default_for_tests();
    let config =
        extract_lint_config(&project, &lint_overrides).expect("entry lint config is valid");
    assert_eq!(config.deny, vec![L001]);
    assert!(config.allow.is_empty());
    assert_eq!(config.fail_on, FailOn::Error);

    let options_overrides = CliOverrides {
        entry: Some("main.main".to_string()),
        ..CliOverrides::default_for_tests()
    };
    let opts = extract_options(&project, &options_overrides).expect("entry options are valid");
    let report = lint_project(&project, &opts, &config);
    assert!(report.findings.iter().all(|finding| finding
        .file
        .as_deref()
        .unwrap()
        .file_name()
        .unwrap()
        == "main.yml"));
}

#[test]
fn static_lint_does_not_execute_shell_or_ipc_payloads() {
    let root = fixture_path("static/no-side-effects");
    let project = ymx_lib::load_project(&root).expect("static fixture loads");
    let options_overrides = CliOverrides {
        entry: Some("main.main".to_string()),
        ..CliOverrides::default_for_tests()
    };
    let mut opts = extract_options(&project, &options_overrides).expect("static options are valid");
    let executor: Arc<dyn CommandExecutor> = Arc::new(StdExecutor);
    opts.executor = Some(Arc::clone(&executor));
    opts.ipc = Some(Arc::new(StdIpcHost::new(executor)));

    let lint_overrides = LintOverrides::default_for_tests();
    let config =
        extract_lint_config(&project, &lint_overrides).expect("static lint config is valid");
    let report = lint_project(&project, &opts, &config);

    let fixture = Fixture {
        root: root.clone(),
        report,
        config,
    };
    assert_eq!(
        tuples(&fixture),
        vec![
            with_code(warning("main.yml", 1, 1, "marker"), L006),
            with_code(warning("main.yml", 9, 3, "main"), L003),
        ]
    );
    assert!(!fixture
        .report
        .diagnostics
        .iter()
        .any(|diag| diag.code == "E016" || diag.code == "E018"));
    assert!(fixture
        .report
        .findings
        .iter()
        .all(|finding| !finding.code.starts_with('E')));

    let marker = "__ymx_lint_side_effect__";
    assert!(!root.join(marker).exists());
    assert!(!Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(marker)
        .exists());
    assert!(!std::env::current_dir()
        .expect("current directory")
        .join(marker)
        .exists());
}

#[test]
fn renderers_produce_exact_output() {
    let mut fixture = load("rules/L006/positive", &LintOverrides::default_for_tests());
    for finding in &mut fixture.report.findings {
        finding.file = Some(PathBuf::from("main.yml"));
    }

    assert_eq!(
        render_diagnostics(&fixture.report),
        "[L006] warning: main.yml:1:1 (echo): IPC alias `echo` is never called\n  help: remove the `_use` entry, or add a call site."
    );
    assert_eq!(
        render_json(&fixture.report).expect("json render"),
        "{\"findings\":[{\"code\":\"L006\",\"severity\":\"warning\",\"file\":\"main.yml\",\"line\":1,\"col\":1,\"component\":\"echo\",\"message\":\"IPC alias `echo` is never called\",\"help\":\"remove the `_use` entry, or add a call site.\"}],\"diagnostics\":[],\"summary\":{\"findings\":1,\"errors\":0,\"warnings\":1,\"info\":0}}"
    );
}
