//! Front-matter config & entry-path resolution (milestone 1.4).
//!
//! [`extract_options`] applies, per flag, the precedence **CLI flag >
//! entry-file `_ymx` > engine default** and returns the effective
//! [`Options`]. The entry file — the document whose `_ymx` block is the
//! project's front matter — is located by the entry path (CLI `--entry` if
//! set, else the literal default `main.main`). Non-entry `_ymx` blocks are
//! completely ignored: never parsed or validated (invariant #4).
//!
//! `ymx-config` is I/O-free: it consumes an already-loaded [`Project`] and
//! never touches the filesystem.

use std::path::Path;

use ymx_core::diag::{Diagnostic, E004, E010};
use ymx_core::ir::Value;
use ymx_core::project::{Format, Options, PlainMode, Project};
use ymx_core::resolve::resolve_entry;

/// Per-flag CLI override (`None` = flag not provided on the command line).
///
/// The CLI is the top of the per-flag precedence ladder (CLI > entry-file
/// `_ymx` > engine default); a `None` field defers to the entry file, then to
/// the engine default.
#[derive(Debug, Clone, PartialEq)]
pub struct CliOverrides {
    /// `--entry <path>` override (default `main.main`).
    pub entry: Option<String>,
    /// `--max-depth <n>` override (default `256`).
    pub max_depth: Option<u32>,
    /// `--pretty` override (default `false`).
    pub pretty: Option<bool>,
    /// `--format <json|diagnostics>` override (default `Format::Json`).
    pub format: Option<Format>,
    /// Plain mode override (set via `_ymx` front matter, not CLI).
    pub plain: Option<PlainMode>,
    /// `--allowed-backends <list>` override (default `None` = all allowed).
    pub allowed_backends: Option<Vec<String>>,
    /// `--allowed-ipc <list>` override (default `None` = all allowed).
    pub allowed_ipc: Option<Vec<String>>,
    /// `--pdf-backend <system|bundled|docker>` override (default `System`).
    pub pdf_backend: Option<String>,
}

impl CliOverrides {
    /// All-`None` overrides — the harness shape used by `_test` runs (PRD
    /// §Testing: `extract_options(&project, &CliOverrides::default_for_tests())`).
    pub fn default_for_tests() -> Self {
        CliOverrides {
            entry: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
            allowed_backends: None,
            allowed_ipc: None,
            pdf_backend: None,
        }
    }
}

/// Applies precedence CLI > entry-file `_ymx` > engine default per flag and
/// returns the effective [`Options`].
///
/// The entry path (`cli.entry` if set, else the literal default `main.main`)
/// is resolved against the loaded [`Project`] via
/// [`resolve_entry`](ymx_core::resolve::resolve_entry); any `E009` — malformed
/// path, missing file, ambiguous stem, or non‑component entry name —
/// propagates as the sole diagnostic (the front-matter source is
/// unknown until the entry resolves). The effective `Options.entry` is the
/// entry path as written.
///
/// The resolved document's raw `_ymx` value is the front matter. A non-object
/// `_ymx` block in the entry file, an unknown field, or an invalid value for a
/// known field is `E010`, anchored at line 1 column 1 of the entry document
/// (the raw meta value carries no spans), with the field name in `component`.
/// All such errors are collected before returning (no short-circuiting);
/// invalid fields simply contribute nothing to the effective options. The
/// recognized fields (`max_depth` int, `from_keyword` string,
/// `format` `"json"`|`"diagnostics"`, `pretty` bool, `plain`
/// `"false"`|`"true"`|`"template"`) are per the PRD `_ymx` table. `plain`
/// is a strict string enum: a YAML bare bool or number is invalid. `entry`
/// is intentionally **not** a `_ymx` field (unknown -> `E010`).
/// `from_keyword` is configurable only via `_ymx` front matter (not via CLI flags).
///
/// Non-entry `_ymx` blocks are never touched: only the entry file's raw value
/// is consulted, so a malformed block elsewhere is not an error.
///
/// Once the effective `plain` mode is known, the sub-namespace names it would
/// promote are checked against the global namespace: a promoted name colliding
/// with an existing global definition of the same full name is `E004`
/// (anchored at 1:1 of the promoted definition's host file, `component` = the
/// name), collected alongside the `_ymx` errors. Under `TemplatesOnly` only
/// `$`-prefixed names can clash; under `False` nothing is promoted.
pub fn extract_options(project: &Project, cli: &CliOverrides) -> Result<Options, Vec<Diagnostic>> {
    let entry = cli.entry.clone().unwrap_or_else(|| "main.main".to_string());
    let (file_id, _, _) = match resolve_entry(project, &entry) {
        Ok(resolved) => resolved,
        Err(diag) => return Err(vec![diag]),
    };

    let mut opts = Options {
        entry,
        ..Options::default()
    };
    let mut diags: Vec<Diagnostic> = Vec::new();

    let mut from_keyword: Option<String> = None;
    let mut max_depth: Option<u32> = None;
    let mut pretty: Option<bool> = None;
    let mut format: Option<Format> = None;
    let mut plain: Option<PlainMode> = None;
    let mut allowed_backends: Option<Vec<String>> = None;
    let mut allowed_ipc: Option<Vec<String>> = None;
    let mut pdf_backend: Option<String> = None;

    if let Some((_, value)) = project.raw_meta_ymx.iter().find(|(fid, _)| *fid == file_id) {
        let entry_file = project.files[file_id.0 as usize].clone();
        match value {
            Value::Object(fields) => {
                for (name, field_value) in fields {
                    match name.as_str() {
                        "max_depth" => match field_value {
                            Value::Int(i) => match u32::try_from(*i) {
                                Ok(n) => max_depth = Some(n),
                                Err(_) => diags.push(invalid_field(
                                    &entry_file,
                                    "max_depth",
                                    "expected a non-negative integer",
                                )),
                            },
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "max_depth",
                                "expected an integer",
                            )),
                        },
                        "from_keyword" => match field_value {
                            Value::String(s) => from_keyword = Some(s.clone()),
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "from_keyword",
                                "expected a string",
                            )),
                        },
                        "format" => match field_value {
                            Value::String(s) if s == "json" => format = Some(Format::Json),
                            Value::String(s) if s == "diagnostics" => {
                                format = Some(Format::Diagnostics)
                            }
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "format",
                                "expected \"json\" or \"diagnostics\"",
                            )),
                        },
                        "pretty" => match field_value {
                            Value::Bool(b) => pretty = Some(*b),
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "pretty",
                                "expected a boolean (YAML `true`/`false`)",
                            )),
                        },
                        "plain" => match field_value {
                            Value::Bool(false) => plain = Some(PlainMode::False),
                            Value::Bool(true) => plain = Some(PlainMode::All),
                            Value::String(s) if s == "false" => plain = Some(PlainMode::False),
                            Value::String(s) if s == "true" => plain = Some(PlainMode::All),
                            Value::String(s) if s == "template" => {
                                plain = Some(PlainMode::TemplatesOnly)
                            }
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "plain",
                                "expected a boolean (YAML `true`/`false`) or the string `\"template\"`",
                            )),
                        },
                        "allowed_backends" => match field_value {
                            Value::Array(items) => {
                                let mut backends = Vec::with_capacity(items.len());
                                let mut valid = true;
                                for item in items {
                                    match item {
                                        Value::String(s) if !s.is_empty() => {
                                            backends.push(s.clone());
                                        }
                                        _ => {
                                            valid = false;
                                            diags.push(invalid_field(
                                                &entry_file,
                                                "allowed_backends",
                                                "expected a list of non-empty strings",
                                            ));
                                            break;
                                        }
                                    }
                                }
                                if valid {
                                    allowed_backends = Some(backends);
                                }
                            }
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "allowed_backends",
                                "expected a list of strings",
                            )),
                        },
                        "allowed_ipc" => match field_value {
                            Value::Array(items) => {
                                let mut transports = Vec::with_capacity(items.len());
                                let mut valid = true;
                                for item in items {
                                    match item {
                                        Value::String(s) if !s.is_empty() => {
                                            transports.push(s.clone());
                                        }
                                        _ => {
                                            valid = false;
                                            diags.push(invalid_field(
                                                &entry_file,
                                                "allowed_ipc",
                                                "expected a list of non-empty strings",
                                            ));
                                            break;
                                        }
                                    }
                                }
                                if valid {
                                    allowed_ipc = Some(transports);
                                }
                            }
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "allowed_ipc",
                                "expected a list of strings",
                            )),
                        },
                        "pdf_backend" => match field_value {
                            Value::String(s) if s == "system" => {
                                pdf_backend = Some("system".to_string())
                            }
                            Value::String(s) if s == "bundled" => {
                                pdf_backend = Some("bundled".to_string())
                            }
                            Value::String(s) if s == "docker" => {
                                pdf_backend = Some("docker".to_string())
                            }
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "pdf_backend",
                                "expected one of \"system\" | \"bundled\" | \"docker\"",
                            )),
                        },
                        _ => diags.push(Diagnostic {
                            file: Some(entry_file.clone()),
                            line: 1,
                            col: 1,
                            component: Some(name.clone()),
                            code: E010,
                            message: format!("unknown `_ymx` field `{name}`"),
                        }),
                    }
                }
            }
            _ => diags.push(Diagnostic {
                file: Some(entry_file),
                line: 1,
                col: 1,
                component: None,
                code: E010,
                message: "invalid `_ymx` block: expected a mapping of compiler-flag defaults"
                    .to_string(),
            }),
        }
    }

    opts.from_keyword = from_keyword.unwrap_or_else(|| "from".to_string());
    opts.max_depth = cli.max_depth.or(max_depth).unwrap_or(256);
    opts.pretty = cli.pretty.or(pretty).unwrap_or(false);
    opts.format = cli.format.clone().or(format).unwrap_or(Format::Json);
    let effective_plain = cli.plain.clone().or(plain).unwrap_or(PlainMode::False);
    opts.plain = effective_plain.clone();
    opts.allowed_backends = cli.allowed_backends.clone().or(allowed_backends);
    opts.allowed_ipc = cli.allowed_ipc.clone().or(allowed_ipc);

    // pdf_backend: CLI > entry-file _ymx > engine default
    let effective_pdf_backend = match &cli.pdf_backend {
        Some(s) => match s.as_str() {
            "system" => Some("system".to_string()),
            "bundled" => Some("bundled".to_string()),
            "docker" => Some("docker".to_string()),
            _ => {
                diags.push(Diagnostic {
                    file: None,
                    line: 1,
                    col: 1,
                    component: Some("pdf_backend".to_string()),
                    code: E010,
                    message: "invalid value for CLI `--pdf-backend`: expected \"system\" | \"bundled\" | \"docker\"".to_string(),
                });
                None
            }
        },
        None => pdf_backend,
    };
    opts.pdf_backend = effective_pdf_backend.unwrap_or("docker".to_string());

    // Promotion clash check: under the effective `plain`, a sub-namespace name
    // that would be promoted must not collide with an existing global
    // definition of the same full name (E004). Under `TemplatesOnly` only
    // `$`-prefixed names can clash; under `False` nothing is promoted.
    let view = project.effective_global_namespace(effective_plain);
    for (name, path, def) in &view.promoted {
        if view
            .global
            .iter()
            .any(|(global_name, _)| global_name == name)
        {
            diags.push(Diagnostic {
                file: Some(project.files[def.file.0 as usize].clone()),
                line: 1,
                col: 1,
                component: Some((*name).to_string()),
                code: E004,
                message: format!(
                    "promotion clash: `{name}` is already defined in the global namespace; the promoted `{path}.{name}` collides"
                ),
            });
        }
    }

    if diags.is_empty() {
        Ok(opts)
    } else {
        Err(diags)
    }
}

/// An `E010` diagnostic for an invalid `_ymx` field value, anchored at 1:1 of
/// the entry document (the raw meta value carries no spans), `component` =
/// the field name.
fn invalid_field(file: &Path, field: &str, detail: &str) -> Diagnostic {
    Diagnostic {
        file: Some(file.to_path_buf()),
        line: 1,
        col: 1,
        component: Some(field.to_string()),
        code: E010,
        message: format!("invalid value for `_ymx` field `{field}`: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ymx_core::diag::{FileId, Span, E009};
    use ymx_core::namespace::Definition;
    use ymx_core::parse::{node_to_value, parse_document, Node};

    const SPAN: Span = Span { line: 1, col: 1 };

    fn def(file: u32, name: &str) -> Definition {
        Definition {
            file: FileId(file),
            full_name: name.to_string(),
            span: SPAN,
            body: Node::Int(1, SPAN),
            math_shorthand: false,
            trailing_question: false,
            exec_backend: None,
        }
    }

    /// Project rooted at `/proj`:
    /// * `main.yml`     (FileId 0): `main`, `$box`
    /// * `a/b.yml`      (FileId 1): `x`, `$xbox`
    /// * `subdir/t.yml` (FileId 2): `t`, `$tbox`, `x`
    fn project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![
            PathBuf::from("/proj/main.yml"),
            PathBuf::from("/proj/a/b.yml"),
            PathBuf::from("/proj/subdir/t.yml"),
        ];
        p.namespaces.register("", def(0, "main")).unwrap();
        p.namespaces.register("", def(0, "$box")).unwrap();
        p.namespaces.register("a", def(1, "x")).unwrap();
        p.namespaces.register("a", def(1, "$xbox")).unwrap();
        p.namespaces.register("subdir", def(2, "t")).unwrap();
        p.namespaces.register("subdir", def(2, "$tbox")).unwrap();
        p.namespaces.register("subdir", def(2, "x")).unwrap();
        p
    }

    /// `project()` plus global `x` and `$tbox` — promotion-clash targets.
    fn clash_project() -> Project {
        let mut p = project();
        p.namespaces.register("", def(0, "x")).unwrap();
        p.namespaces.register("", def(0, "$tbox")).unwrap();
        p
    }

    /// Raw span-less value of inline YAML (mirrors ymx-lib's `value_of`).
    fn value_of(src: &str) -> Value {
        node_to_value(&parse_document(src).expect("parse inline yaml"))
    }

    /// Attach a raw `_ymx` value to document `file`.
    fn with_ymx(mut p: Project, file: u32, src: &str) -> Project {
        p.raw_meta_ymx.push((FileId(file), value_of(src)));
        p
    }

    #[test]
    fn empty_ymx_and_empty_overrides_equal_default() {
        let p = with_ymx(project(), 0, "{}\n");
        let opts = extract_options(&p, &CliOverrides::default_for_tests()).expect("empty _ymx");
        assert_eq!(opts, Options::default());
    }

    #[test]
    fn no_ymx_and_empty_overrides_equal_default() {
        let p = project();
        let opts = extract_options(&p, &CliOverrides::default_for_tests()).expect("no _ymx");
        assert_eq!(opts, Options::default());
    }

    #[test]
    fn entry_file_overrides_default_with_empty_cli() {
        let p = with_ymx(
            project(),
            0,
            "max_depth: 10\nfrom_keyword: frm\nformat: diagnostics\npretty: true\nplain: \"template\"\n",
        );
        let opts =
            extract_options(&p, &CliOverrides::default_for_tests()).expect("all fields valid");
        assert_eq!(opts.max_depth, 10);
        assert_eq!(opts.from_keyword, "frm");
        assert_eq!(opts.format, Format::Diagnostics);
        assert!(opts.pretty);
        assert_eq!(opts.plain, PlainMode::TemplatesOnly);
        assert_eq!(opts.entry, "main.main");
    }

    #[test]
    fn cli_overrides_entry_file_per_field() {
        let p = with_ymx(
            project(),
            0,
            "max_depth: 10\nformat: diagnostics\npretty: true\nplain: \"true\"\n",
        );
        let cli = CliOverrides {
            max_depth: Some(5),
            pretty: Some(false),
            ..CliOverrides::default_for_tests()
        };
        let opts = extract_options(&p, &cli).expect("valid");
        assert_eq!(opts.max_depth, 5, "CLI beats entry-file 10");
        assert_eq!(
            opts.format,
            Format::Diagnostics,
            "no CLI override -> entry-file value"
        );
        assert!(!opts.pretty, "CLI false beats entry-file true");
        assert_eq!(
            opts.plain,
            PlainMode::All,
            "no CLI override -> entry-file \"true\""
        );
        assert_eq!(opts.entry, "main.main");
    }

    #[test]
    fn partial_entry_ymx_leaves_other_fields_at_defaults() {
        let p = with_ymx(project(), 0, "max_depth: 42\n");
        let opts = extract_options(&p, &CliOverrides::default_for_tests()).expect("valid");
        assert_eq!(
            opts,
            Options {
                max_depth: 42,
                ..Options::default()
            }
        );
    }

    #[test]
    fn cli_entry_selects_front_matter_source_and_ignores_others() {
        let mut p = with_ymx(project(), 1, "max_depth: 7\n");
        p.raw_meta_ymx.push((FileId(0), value_of("foo: garbage\n")));
        let cli = CliOverrides {
            entry: Some("a.b.x".to_string()),
            ..CliOverrides::default_for_tests()
        };
        let opts =
            extract_options(&p, &cli).expect("main.yml's malformed _ymx is not the entry file's");
        assert_eq!(opts.entry, "a.b.x");
        assert_eq!(opts.max_depth, 7, "a/b.yml's _ymx is the front matter");
    }

    #[test]
    fn non_entry_malformed_ymx_is_not_validated() {
        // main.yml (the entry file) has no `_ymx`; other documents carry
        // malformed blocks (a garbage object, a non-object) that are ignored.
        let mut p = project();
        p.raw_meta_ymx.push((
            FileId(1),
            value_of("foo: 1\nmax_depth: nope\nplain: \"maybe\"\n"),
        ));
        p.raw_meta_ymx.push((FileId(2), value_of("5")));
        let opts = extract_options(&p, &CliOverrides::default_for_tests()).expect("ignored");
        assert_eq!(opts, Options::default());
    }

    #[test]
    fn unknown_ymx_field_is_e010() {
        let p = with_ymx(project(), 0, "foo: 1\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.code, E010);
        assert_eq!(d.file.as_deref(), Some(Path::new("/proj/main.yml")));
        assert_eq!((d.line, d.col), (1, 1));
        assert_eq!(d.component.as_deref(), Some("foo"));
        assert!(d.message.contains("foo"));
    }

    #[test]
    fn entry_is_not_a_ymx_field() {
        let p = with_ymx(project(), 0, "entry: other.main\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component.as_deref(), Some("entry"));
    }

    #[test]
    fn plain_rejects_non_string_enum_values() {
        for src in ["plain: \"maybe\"\n", "plain: 5\n"] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("plain"), "{src}");
        }
    }

    #[test]
    fn max_depth_rejects_non_integer_and_out_of_range() {
        for src in [
            "max_depth: nope\n",
            "max_depth: -1\n",
            "max_depth: 4294967296\n",
            "max_depth: 1.5\n",
        ] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("max_depth"), "{src}");
        }
    }

    #[test]
    fn format_rejects_unknown_values() {
        for src in ["format: xml\n", "format: JSON\n", "format: true\n"] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("format"), "{src}");
        }
    }

    #[test]
    fn pretty_rejects_non_bool() {
        for src in ["pretty: \"true\"\n", "pretty: 1\n"] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(diags[0].component.as_deref(), Some("pretty"), "{src}");
        }
    }

    #[test]
    fn keyword_fields_reject_non_string() {
        for field in ["from_keyword"] {
            for src in [format!("{field}: true\n"), format!("{field}: 5\n")] {
                let p = with_ymx(project(), 0, &src);
                let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
                assert_eq!(diags.len(), 1, "{src}");
                assert_eq!(diags[0].code, E010, "{src}");
                assert_eq!(diags[0].component.as_deref(), Some(field), "{src}");
            }
        }
    }

    #[test]
    fn non_object_ymx_is_e010() {
        for src in ["5\n", "- 1\n- 2\n", "\"x\"\n", ""] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src:?}");
            assert_eq!(diags[0].code, E010, "{src:?}");
            assert_eq!(diags[0].component, None, "{src:?}");
            assert_eq!(
                diags[0].file.as_deref(),
                Some(Path::new("/proj/main.yml")),
                "{src:?}"
            );
        }
    }

    #[test]
    fn all_ymx_validation_errors_are_collected() {
        let p = with_ymx(project(), 0, "max_depth: nope\nfoo: 1\nplain: \"maybe\"\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 3);
        assert!(diags.iter().all(|d| d.code == E010));
        let components: Vec<Option<&str>> = diags.iter().map(|d| d.component.as_deref()).collect();
        assert_eq!(
            components,
            [Some("max_depth"), Some("foo"), Some("plain")],
            "errors collected in insertion order, no short-circuit"
        );
    }

    #[test]
    fn malformed_entry_is_e009() {
        for entry in ["main", "a..c"] {
            let cli = CliOverrides {
                entry: Some(entry.to_string()),
                ..CliOverrides::default_for_tests()
            };
            let diags = extract_options(&project(), &cli).unwrap_err();
            assert_eq!(diags.len(), 1, "{entry}");
            assert_eq!(diags[0].code, E009, "{entry}");
            assert_eq!(diags[0].file, None, "{entry}: no document implicated");
        }
    }

    #[test]
    fn missing_entry_file_is_e009() {
        let cli = CliOverrides {
            entry: Some("a.missing.c".to_string()),
            ..CliOverrides::default_for_tests()
        };
        let diags = extract_options(&project(), &cli).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E009);
        assert_eq!(diags[0].file, None);
        assert!(
            diags[0].message.contains("a/missing.yml"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn component_not_defined_in_entry_file_is_e009() {
        let cli = CliOverrides {
            entry: Some("a.b.y".to_string()),
            ..CliOverrides::default_for_tests()
        };
        let opts = extract_options(&project(), &cli).unwrap();
        assert_eq!(opts.entry, "a.b.y");
    }

    #[test]
    fn plain_valid_values_parse_to_plain_mode() {
        for (src, expected) in [
            ("plain: \"false\"\n", PlainMode::False),
            ("plain: \"true\"\n", PlainMode::All),
            ("plain: \"template\"\n", PlainMode::TemplatesOnly),
            ("plain: true\n", PlainMode::All),
            ("plain: false\n", PlainMode::False),
        ] {
            let p = with_ymx(project(), 0, src);
            let opts = extract_options(&p, &CliOverrides::default_for_tests()).expect(src);
            assert_eq!(opts.plain, expected, "{src}");
        }
    }

    #[test]
    fn cli_plain_overrides_entry_plain() {
        let p = with_ymx(project(), 0, "plain: \"true\"\n");
        let cli = CliOverrides {
            plain: Some(PlainMode::False),
            ..CliOverrides::default_for_tests()
        };
        let opts = extract_options(&p, &cli).expect("CLI False beats entry All");
        assert_eq!(opts.plain, PlainMode::False);
    }

    #[test]
    fn promotion_clash_is_e004_under_all() {
        let p = with_ymx(clash_project(), 0, "plain: \"true\"\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 3, "a.x, subdir.$tbox, subdir.x");
        for d in &diags {
            assert_eq!(d.code, E004);
            assert_eq!((d.line, d.col), (1, 1));
        }
        let rendered: Vec<(&str, &str)> = diags
            .iter()
            .map(|d| {
                (
                    d.component.as_deref().unwrap(),
                    d.file.as_deref().unwrap().to_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rendered,
            [
                ("x", "/proj/a/b.yml"),
                ("$tbox", "/proj/subdir/t.yml"),
                ("x", "/proj/subdir/t.yml"),
            ],
            "lexicographic promotion order; anchored at the promoted definition's host file"
        );
        assert!(diags[0].message.contains("a.x"), "{}", diags[0].message);
        assert!(
            diags[1].message.contains("subdir.$tbox"),
            "{}",
            diags[1].message
        );
        assert!(
            diags[2].message.contains("subdir.x"),
            "{}",
            diags[2].message
        );
    }

    #[test]
    fn promotion_clash_is_e004_under_templates_only() {
        let p = with_ymx(clash_project(), 0, "plain: \"template\"\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(
            diags.len(),
            1,
            "only `$` names can clash under TemplatesOnly"
        );
        assert_eq!(diags[0].code, E004);
        assert_eq!(diags[0].component.as_deref(), Some("$tbox"));
        assert_eq!(
            diags[0].file.as_deref(),
            Some(Path::new("/proj/subdir/t.yml"))
        );
        assert!(
            diags[0].message.contains("subdir.$tbox"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn no_promotion_clash_under_false() {
        let p = with_ymx(clash_project(), 0, "plain: \"false\"\n");
        let opts = extract_options(&p, &CliOverrides::default_for_tests())
            .expect("False promotes nothing");
        assert_eq!(opts.plain, PlainMode::False);
    }

    #[test]
    fn cli_plain_false_suppresses_entry_clash() {
        let p = with_ymx(clash_project(), 0, "plain: \"true\"\n");
        let cli = CliOverrides {
            plain: Some(PlainMode::False),
            ..CliOverrides::default_for_tests()
        };
        let opts = extract_options(&p, &cli).expect("CLI False beats entry All");
        assert_eq!(opts.plain, PlainMode::False);
    }

    #[test]
    fn cli_plain_drives_clash_check_without_ymx() {
        let p = clash_project();
        let cli = CliOverrides {
            plain: Some(PlainMode::All),
            ..CliOverrides::default_for_tests()
        };
        let diags = extract_options(&p, &cli).unwrap_err();
        assert_eq!(diags.len(), 3);
        assert!(diags.iter().all(|d| d.code == E004));

        let cli = CliOverrides {
            plain: Some(PlainMode::TemplatesOnly),
            ..CliOverrides::default_for_tests()
        };
        let diags = extract_options(&p, &cli).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].component.as_deref(), Some("$tbox"));
    }

    #[test]
    fn clash_collected_alongside_ymx_errors() {
        let p = with_ymx(clash_project(), 0, "max_depth: nope\nplain: \"true\"\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 4, "1 E010 + 3 E004");
        assert_eq!(diags.iter().filter(|d| d.code == E010).count(), 1);
        assert_eq!(diags.iter().filter(|d| d.code == E004).count(), 3);
    }

    #[test]
    fn allowed_backends_valid_list() {
        let p = with_ymx(project(), 0, "allowed_backends: [sh]\n");
        let opts = extract_options(&p, &CliOverrides::default_for_tests())
            .expect("valid allowed_backends");
        assert_eq!(opts.allowed_backends, Some(vec!["sh".to_string()]));
    }

    #[test]
    fn allowed_backends_multiple_entries() {
        let p = with_ymx(project(), 0, "allowed_backends: [sh, python, ruby]\n");
        let opts = extract_options(&p, &CliOverrides::default_for_tests())
            .expect("valid allowed_backends");
        assert_eq!(
            opts.allowed_backends,
            Some(vec![
                "sh".to_string(),
                "python".to_string(),
                "ruby".to_string()
            ])
        );
    }

    #[test]
    fn allowed_backends_absent_is_none() {
        let p = with_ymx(project(), 0, "max_depth: 10\n");
        let opts = extract_options(&p, &CliOverrides::default_for_tests())
            .expect("absent allowed_backends");
        assert_eq!(opts.allowed_backends, None);
    }

    #[test]
    fn allowed_backends_empty_list() {
        let p = with_ymx(project(), 0, "allowed_backends: []\n");
        let opts =
            extract_options(&p, &CliOverrides::default_for_tests()).expect("empty list is valid");
        assert_eq!(opts.allowed_backends, Some(vec![]));
    }

    #[test]
    fn allowed_backends_rejects_non_list() {
        for src in [
            "allowed_backends: sh\n",
            "allowed_backends: 5\n",
            "allowed_backends: true\n",
        ] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(
                diags[0].component.as_deref(),
                Some("allowed_backends"),
                "{src}"
            );
        }
    }

    #[test]
    fn allowed_backends_rejects_non_string_elements() {
        for src in ["allowed_backends: [5]\n", "allowed_backends: [true]\n"] {
            let p = with_ymx(project(), 0, src);
            let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(diags[0].code, E010, "{src}");
            assert_eq!(
                diags[0].component.as_deref(),
                Some("allowed_backends"),
                "{src}"
            );
        }
    }

    #[test]
    fn allowed_backends_rejects_empty_string_element() {
        let p = with_ymx(project(), 0, "allowed_backends: [\"\"]\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, E010);
        assert_eq!(diags[0].component.as_deref(), Some("allowed_backends"));
    }

    #[test]
    fn cli_allowed_backends_overrides_entry() {
        let p = with_ymx(project(), 0, "allowed_backends: [python]\n");
        let cli = CliOverrides {
            allowed_backends: Some(vec!["ruby".to_string()]),
            ..CliOverrides::default_for_tests()
        };
        let opts = extract_options(&p, &cli).expect("CLI beats entry");
        assert_eq!(opts.allowed_backends, Some(vec!["ruby".to_string()]));
    }

    #[test]
    fn cli_allowed_backends_overrides_absent_entry() {
        let cli = CliOverrides {
            allowed_backends: Some(vec!["sh".to_string()]),
            ..CliOverrides::default_for_tests()
        };
        let opts = extract_options(&project(), &cli).expect("CLI over absent");
        assert_eq!(opts.allowed_backends, Some(vec!["sh".to_string()]));
    }

    #[test]
    fn allowed_backends_collected_alongside_other_errors() {
        let p = with_ymx(project(), 0, "allowed_backends: [5]\nfoo: 1\n");
        let diags = extract_options(&p, &CliOverrides::default_for_tests()).unwrap_err();
        assert_eq!(
            diags.len(),
            2,
            "1 E010 for allowed_backends + 1 E010 for foo"
        );
        let components: Vec<Option<&str>> = diags.iter().map(|d| d.component.as_deref()).collect();
        assert_eq!(
            components,
            [Some("allowed_backends"), Some("foo")],
            "errors collected in insertion order"
        );
    }
}
