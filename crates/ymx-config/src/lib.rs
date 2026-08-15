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
    /// `--from-keyword <kw>` override (default `from`).
    pub from_keyword: Option<String>,
    /// `--default-keyword <kw>` override (default `default`; the engine
    /// prefixes `$` internally).
    pub default_keyword: Option<String>,
    /// `--max-depth <n>` override (default `256`).
    pub max_depth: Option<u32>,
    /// `--pretty` override (default `false`).
    pub pretty: Option<bool>,
    /// `--format <json|diagnostics>` override (default `Format::Json`).
    pub format: Option<Format>,
    /// `--plain` / `--plain-template` override (default
    /// `PlainMode::False`). `--plain` maps to `PlainMode::All`,
    /// `--plain-template` to `PlainMode::TemplatesOnly`.
    pub plain: Option<PlainMode>,
}

impl CliOverrides {
    /// All-`None` overrides — the harness shape used by `_test` runs (PRD
    /// §Testing: `extract_options(&project, &CliOverrides::default_for_tests())`).
    pub fn default_for_tests() -> Self {
        CliOverrides {
            entry: None,
            from_keyword: None,
            default_keyword: None,
            max_depth: None,
            pretty: None,
            format: None,
            plain: None,
        }
    }
}

/// Applies precedence CLI > entry-file `_ymx` > engine default per flag and
/// returns the effective [`Options`].
///
/// The entry path (`cli.entry` if set, else the literal default `main.main`)
/// is resolved against the loaded [`Project`] via
/// [`resolve_entry`](ymx_core::resolve::resolve_entry); any `E009` — malformed
/// path, missing file, ambiguous stem, component not defined in the entry
/// file — propagates as the sole diagnostic (the front-matter source is
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
/// `default_keyword` string, `format` `"json"`|`"diagnostics"`, `pretty`
/// bool, `plain` `"false"`|`"true"`|`"template"`) are per the PRD `_ymx`
/// table. `plain` is a strict string enum: a YAML bare bool or number is
/// invalid. `entry` is intentionally **not** a `_ymx` field (unknown -> `E010`).
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
    let mut default_keyword: Option<String> = None;
    let mut max_depth: Option<u32> = None;
    let mut pretty: Option<bool> = None;
    let mut format: Option<Format> = None;
    let mut plain: Option<PlainMode> = None;

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
                        "default_keyword" => match field_value {
                            Value::String(s) => default_keyword = Some(s.clone()),
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "default_keyword",
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
                            Value::String(s) if s == "false" => plain = Some(PlainMode::False),
                            Value::String(s) if s == "true" => plain = Some(PlainMode::All),
                            Value::String(s) if s == "template" => {
                                plain = Some(PlainMode::TemplatesOnly)
                            }
                            _ => diags.push(invalid_field(
                                &entry_file,
                                "plain",
                                "expected one of \"false\" | \"true\" | \"template\" (a string)",
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

    opts.from_keyword = cli
        .from_keyword
        .clone()
        .or(from_keyword)
        .unwrap_or_else(|| "from".to_string());
    opts.default_keyword = cli
        .default_keyword
        .clone()
        .or(default_keyword)
        .unwrap_or_else(|| "default".to_string());
    opts.max_depth = cli.max_depth.or(max_depth).unwrap_or(256);
    opts.pretty = cli.pretty.or(pretty).unwrap_or(false);
    opts.format = cli.format.clone().or(format).unwrap_or(Format::Json);
    let effective_plain = cli.plain.clone().or(plain).unwrap_or(PlainMode::False);
    opts.plain = effective_plain.clone();

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
