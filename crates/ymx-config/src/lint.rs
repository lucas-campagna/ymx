//! `_ymx.lint` front-matter extraction (milestone 1.48, task 2).
//!
//! Only the entry file's `_ymx.lint` block is read; non-entry blocks are
//! ignored completely. `ymx-config` stays I/O-free and does not depend on
//! `ymx-lint`.

use std::path::Path;

use ymx_core::diag::{Diagnostic, E010};
use ymx_core::ir::Value;
use ymx_core::project::Project;
use ymx_core::resolve::resolve_entry;

/// `L001` — unused component or template.
pub const L001: &str = "L001";
/// `L002` — unreachable component from entry.
pub const L002: &str = "L002";
/// `L003` — unused argument.
pub const L003: &str = "L003";
/// `L004` — argument passed but never read by the target.
pub const L004: &str = "L004";
/// `L005` — shell call with an interpolated payload.
pub const L005: &str = "L005";
/// `L006` — IPC alias declared but never called.
pub const L006: &str = "L006";

const LINT_CODES: [&str; 6] = [L001, L002, L003, L004, L005, L006];

/// Severity of a lint finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
}

/// Minimum emitted lint severity that makes a lint run fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOn {
    Error,
    Warning,
    Info,
    None,
}

/// Effective lint configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintConfig {
    pub deny: Vec<&'static str>,
    pub allow: Vec<&'static str>,
    pub fail_on: FailOn,
}

impl Default for LintConfig {
    fn default() -> Self {
        LintConfig {
            deny: Vec::new(),
            allow: Vec::new(),
            fail_on: FailOn::Error,
        }
    }
}

/// Per-field lint CLI overrides (`None` = flag not provided).
#[derive(Debug, Clone, PartialEq)]
pub struct LintOverrides {
    pub entry: Option<String>,
    pub deny: Option<Vec<String>>,
    pub allow: Option<Vec<String>>,
    pub fail_on: Option<FailOn>,
}

impl LintOverrides {
    /// All-`None` overrides for direct callers and lint config tests.
    pub fn default_for_tests() -> Self {
        LintOverrides {
            entry: None,
            deny: None,
            allow: None,
            fail_on: None,
        }
    }
}

impl Default for LintOverrides {
    fn default() -> Self {
        Self::default_for_tests()
    }
}

/// Resolves the entry file exactly like [`extract_options`](super::extract_options)
/// and extracts `_ymx.lint` from that file only.
///
/// Precedence is per field: CLI override > entry-file value > default
/// (`deny = []`, `allow = []`, `fail_on = Error`). All invalid values are
/// collected as `E010` diagnostics anchored at 1:1 of the entry document.
pub fn extract_lint_config(
    project: &Project,
    cli: &LintOverrides,
) -> Result<LintConfig, Vec<Diagnostic>> {
    let entry = cli.entry.clone().unwrap_or_else(|| "main.main".to_string());
    let (file_id, _, _) = match resolve_entry(project, &entry) {
        Ok(resolved) => resolved,
        Err(diag) => return Err(vec![diag]),
    };
    let entry_file = project.files[file_id.0 as usize].clone();
    let mut diags: Vec<Diagnostic> = Vec::new();

    let mut file_deny: Option<Vec<&'static str>> = None;
    let mut file_allow: Option<Vec<&'static str>> = None;
    let mut file_fail_on: Option<FailOn> = None;

    if let Some((_, Value::Object(fields))) =
        project.raw_meta_ymx.iter().find(|(fid, _)| *fid == file_id)
    {
        if let Some((_, lint_value)) = fields.iter().find(|(name, _)| name.as_str() == "lint") {
            match lint_value {
                Value::Object(lint_fields) => {
                    for (name, value) in lint_fields {
                        match name.as_str() {
                            "deny" => {
                                file_deny = parse_code_list(value, "deny", &entry_file, &mut diags);
                            }
                            "allow" => {
                                file_allow = parse_code_list(value, "allow", &entry_file, &mut diags);
                            }
                            "fail_on" => {
                                file_fail_on = parse_fail_on(value, &entry_file, &mut diags);
                            }
                            _ => diags.push(lint_diag(
                                &entry_file,
                                name,
                                format!("unknown field in `_ymx.lint`: `{name}`"),
                            )),
                        }
                    }
                }
                _ => diags.push(lint_diag(
                    &entry_file,
                    "lint",
                    "invalid value for `_ymx.lint`: expected a mapping of `deny`, `allow`, and `fail_on`".to_string(),
                )),
            }
        }
    }

    let mut conflicts: Vec<&'static str> = Vec::new();
    if let (Some(file_deny), Some(file_allow)) = (&file_deny, &file_allow) {
        conflicts.extend(overlapping_codes(file_deny, file_allow));
    }

    let deny = match &cli.deny {
        Some(raw) => validate_cli_codes(raw, "deny", &entry_file, &mut diags).unwrap_or_default(),
        None => file_deny.unwrap_or_default(),
    };
    let allow = match &cli.allow {
        Some(raw) => validate_cli_codes(raw, "allow", &entry_file, &mut diags).unwrap_or_default(),
        None => file_allow.unwrap_or_default(),
    };
    let fail_on = cli.fail_on.or(file_fail_on).unwrap_or(FailOn::Error);

    conflicts.extend(overlapping_codes(&deny, &allow));
    conflicts.sort_unstable();
    conflicts.dedup();
    for code in conflicts {
        diags.push(lint_diag(
            &entry_file,
            "lint",
            format!("lint code `{code}` is listed in both `deny` and `allow`"),
        ));
    }

    if diags.is_empty() {
        Ok(LintConfig {
            deny,
            allow,
            fail_on,
        })
    } else {
        Err(diags)
    }
}

fn canonical_code(code: &str) -> Option<&'static str> {
    LINT_CODES.iter().copied().find(|known| *known == code)
}

fn parse_code_list(
    value: &Value,
    field: &str,
    file: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<&'static str>> {
    let Value::Array(items) = value else {
        diags.push(invalid_lint_field(
            file,
            field,
            "expected a list of L-codes",
        ));
        return None;
    };
    let before = diags.len();
    let mut codes: Vec<&'static str> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(s) => match canonical_code(s) {
                Some(code) => {
                    if !codes.contains(&code) {
                        codes.push(code);
                    }
                }
                None => diags.push(invalid_lint_field(
                    file,
                    field,
                    &format!("unknown lint code `{s}`"),
                )),
            },
            _ => diags.push(invalid_lint_field(
                file,
                field,
                "expected a list of L-code strings",
            )),
        }
    }
    if diags.len() == before {
        Some(codes)
    } else {
        None
    }
}

fn validate_cli_codes(
    raw: &[String],
    field: &str,
    file: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<&'static str>> {
    let before = diags.len();
    let mut codes: Vec<&'static str> = Vec::with_capacity(raw.len());
    for value in raw {
        match canonical_code(value) {
            Some(code) => {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            None => diags.push(lint_diag(
                file,
                field,
                format!("invalid value for CLI `--{field}`: unknown lint code `{value}`"),
            )),
        }
    }
    if diags.len() == before {
        Some(codes)
    } else {
        None
    }
}

fn parse_fail_on(value: &Value, file: &Path, diags: &mut Vec<Diagnostic>) -> Option<FailOn> {
    let Value::String(s) = value else {
        diags.push(invalid_lint_field(
            file,
            "fail_on",
            "expected one of \"error\" | \"warning\" | \"info\" | \"none\"",
        ));
        return None;
    };
    match s.as_str() {
        "error" => Some(FailOn::Error),
        "warning" => Some(FailOn::Warning),
        "info" => Some(FailOn::Info),
        "none" => Some(FailOn::None),
        _ => {
            diags.push(invalid_lint_field(
                file,
                "fail_on",
                "expected one of \"error\" | \"warning\" | \"info\" | \"none\"",
            ));
            None
        }
    }
}

fn overlapping_codes(deny: &[&'static str], allow: &[&'static str]) -> Vec<&'static str> {
    deny.iter()
        .copied()
        .filter(|code| allow.contains(code))
        .collect()
}

fn invalid_lint_field(file: &Path, field: &str, detail: &str) -> Diagnostic {
    lint_diag(
        file,
        field,
        format!("invalid value for `_ymx.lint` field `{field}`: {detail}"),
    )
}

fn lint_diag(file: &Path, component: &str, message: String) -> Diagnostic {
    Diagnostic {
        file: Some(file.to_path_buf()),
        line: 1,
        col: 1,
        component: Some(component.to_string()),
        code: E010,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ymx_core::diag::{FileId, E009};
    use ymx_core::parse::{node_to_value, parse_document};

    fn project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![
            PathBuf::from("/proj/main.yml"),
            PathBuf::from("/proj/a/b.yml"),
            PathBuf::from("/proj/subdir/t.yml"),
        ];
        p
    }

    fn value_of(src: &str) -> Value {
        node_to_value(&parse_document(src).expect("parse inline yaml"))
    }

    fn with_ymx(file: u32, src: &str) -> Project {
        let mut p = project();
        p.raw_meta_ymx.push((FileId(file), value_of(src)));
        p
    }

    fn with_lint(file: u32, body: &str) -> Project {
        with_ymx(file, &format!("lint:\n  {body}"))
    }

    fn error_components(diags: &[Diagnostic]) -> Vec<Option<&str>> {
        diags.iter().map(|d| d.component.as_deref()).collect()
    }

    #[test]
    fn defaults_with_no_ymx_or_empty_lint() {
        let expected = LintConfig::default();
        assert_eq!(expected.deny, Vec::<&str>::new());
        assert_eq!(expected.allow, Vec::<&str>::new());
        assert_eq!(expected.fail_on, FailOn::Error);

        for p in [project(), with_ymx(0, "lint: {}\n")] {
            let cfg = extract_lint_config(&p, &LintOverrides::default_for_tests()).expect("valid");
            assert_eq!(cfg, expected);
        }
    }

    #[test]
    fn entry_file_lint_is_parsed() {
        let p = with_lint(
            0,
            "deny: [L001, L003]\n  allow: [L005]\n  fail_on: warning\n",
        );
        let cfg = extract_lint_config(&p, &LintOverrides::default_for_tests()).expect("valid");
        assert_eq!(cfg.deny, [L001, L003]);
        assert_eq!(cfg.allow, [L005]);
        assert_eq!(cfg.fail_on, FailOn::Warning);
    }

    #[test]
    fn all_fail_on_values_are_parsed() {
        for (src, expected) in [
            ("error\n", FailOn::Error),
            ("warning\n", FailOn::Warning),
            ("info\n", FailOn::Info),
            ("none\n", FailOn::None),
        ] {
            let p = with_lint(0, &format!("fail_on: {src}"));
            let cfg = extract_lint_config(&p, &LintOverrides::default_for_tests()).expect(src);
            assert_eq!(cfg.fail_on, expected, "{src}");
        }
    }

    #[test]
    fn non_entry_lint_is_ignored() {
        let mut p = with_ymx(1, "lint:\n  bogus: true\n  deny: [L007]\n");
        p.raw_meta_ymx.push((FileId(2), value_of("lint: 5\n")));
        let cfg = extract_lint_config(&p, &LintOverrides::default_for_tests()).expect("ignored");
        assert_eq!(cfg, LintConfig::default());
    }

    #[test]
    fn cli_entry_selects_the_lint_source() {
        let mut p = with_lint(0, "bogus: true\n");
        p.raw_meta_ymx
            .push((FileId(1), value_of("lint:\n  deny: [L006]\n")));
        let cli = LintOverrides {
            entry: Some("a.b.x".to_string()),
            ..LintOverrides::default_for_tests()
        };
        let cfg = extract_lint_config(&p, &cli).expect("entry file selected");
        assert_eq!(cfg.deny, [L006]);
    }

    #[test]
    fn malformed_entry_is_e009() {
        let cli = LintOverrides {
            entry: Some("main".to_string()),
            ..LintOverrides::default_for_tests()
        };
        let diags = extract_lint_config(&project(), &cli).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E009);
        assert_eq!(diags[0].file, None);
    }

    #[test]
    fn unknown_lint_field_is_e010() {
        let p = with_lint(0, "bogus: 1\n");
        let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component.as_deref(), Some("bogus"));
        assert_eq!(diags[0].file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert_eq!((diags[0].line, diags[0].col), (1, 1));
    }

    #[test]
    fn wrong_value_types_are_e010() {
        for src in [
            "deny: L001\n",
            "allow: [5]\n",
            "allow: [true]\n",
            "fail_on: true\n",
            "fail_on: 5\n",
        ] {
            let p = with_lint(0, src);
            let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert!(diags[0].file.is_some(), "{src}");
            assert_eq!((diags[0].line, diags[0].col), (1, 1), "{src}");
        }
    }

    #[test]
    fn non_object_lint_block_is_e010() {
        for src in ["lint: 5\n", "lint:\n  - L001\n", "lint: \"x\"\n"] {
            let p = with_ymx(0, src);
            let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("lint"), "{src}");
        }
    }

    #[test]
    fn unknown_lint_codes_are_e010() {
        for field in ["deny", "allow"] {
            let p = with_lint(0, &format!("{field}: [L001, L007, L999]\n"));
            let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 2, "{field}");
            assert!(diags.iter().all(|d| d.code == E010));
            assert_eq!(
                error_components(&diags),
                [Some(field), Some(field)],
                "{field}"
            );
            assert!(diags[0].message.contains("L007"));
            assert!(diags[1].message.contains("L999"));
        }
    }

    #[test]
    fn invalid_fail_on_values_are_e010() {
        for src in ["bogus\n", "ERROR\n", "\"\"\n"] {
            let p = with_lint(0, &format!("fail_on: {src}"));
            let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("fail_on"), "{src}");
        }
    }

    #[test]
    fn overlapping_deny_and_allow_are_e010() {
        let p = with_lint(0, "deny: [L001, L002]\n  allow: [L002, L003]\n");
        let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component.as_deref(), Some("lint"));
        assert!(diags[0].message.contains("L002"));
    }

    #[test]
    fn all_entry_errors_are_collected() {
        let p = with_lint(
            0,
            "deny: [L007]\n  bogus: 1\n  fail_on: nope\n  allow: [L002, L002]\n",
        );
        let diags = extract_lint_config(&p, &LintOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 3);
        assert_eq!(
            error_components(&diags),
            [Some("deny"), Some("bogus"), Some("fail_on")]
        );
        assert!(diags.iter().all(|d| d.code == E010));
    }

    #[test]
    fn cli_overrides_entry_file_per_field() {
        let p = with_lint(0, "deny: [L001]\n  allow: [L002]\n  fail_on: error\n");
        let cli = LintOverrides {
            deny: Some(vec!["L004".to_string(), "L005".to_string()]),
            allow: Some(vec!["L006".to_string()]),
            fail_on: Some(FailOn::Info),
            ..LintOverrides::default_for_tests()
        };
        let cfg = extract_lint_config(&p, &cli).expect("valid");
        assert_eq!(cfg.deny, [L004, L005]);
        assert_eq!(cfg.allow, [L006]);
        assert_eq!(cfg.fail_on, FailOn::Info);
    }

    #[test]
    fn cli_overrides_without_entry_file() {
        let cli = LintOverrides {
            deny: Some(vec!["L001".to_string()]),
            allow: Some(vec!["L002".to_string(), "L003".to_string()]),
            fail_on: Some(FailOn::None),
            ..LintOverrides::default_for_tests()
        };
        let cfg = extract_lint_config(&project(), &cli).expect("valid");
        assert_eq!(
            cfg,
            LintConfig {
                deny: vec![L001],
                allow: vec![L002, L003],
                fail_on: FailOn::None,
            }
        );
    }

    #[test]
    fn cli_empty_lists_replace_entry_file_lists() {
        let p = with_lint(0, "deny: [L001]\n  allow: [L002]\n  fail_on: warning\n");
        let cli = LintOverrides {
            deny: Some(Vec::new()),
            allow: Some(Vec::new()),
            ..LintOverrides::default_for_tests()
        };
        let cfg = extract_lint_config(&p, &cli).expect("valid");
        assert_eq!(cfg.deny, Vec::<&str>::new());
        assert_eq!(cfg.allow, Vec::<&str>::new());
        assert_eq!(cfg.fail_on, FailOn::Warning);
    }

    #[test]
    fn invalid_cli_codes_are_e010() {
        let cli = LintOverrides {
            deny: Some(vec!["L001".to_string(), "L007".to_string()]),
            allow: Some(vec!["bogus".to_string()]),
            ..LintOverrides::default_for_tests()
        };
        let diags = extract_lint_config(&project(), &cli).unwrap_err();
        assert_eq!(error_components(&diags), [Some("deny"), Some("allow")]);
        assert!(diags.iter().all(|d| d.code == E010));
    }

    #[test]
    fn overlapping_cli_codes_are_e010() {
        let cli = LintOverrides {
            deny: Some(vec!["L003".to_string()]),
            allow: Some(vec!["L003".to_string()]),
            ..LintOverrides::default_for_tests()
        };
        let diags = extract_lint_config(&project(), &cli).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component.as_deref(), Some("lint"));
    }

    #[test]
    fn lint_constants_cover_all_codes() {
        assert_eq!(LINT_CODES, [L001, L002, L003, L004, L005, L006]);
        for code in LINT_CODES {
            assert_eq!(canonical_code(code), Some(code));
        }
        assert_eq!(canonical_code("L007"), None);
    }
}
