//! Integration harness (milestone 1.9): walks `tests/cases/rule-NN/<scenario>/`
//! under the workspace root and runs each scenario's `_test` blocks against a
//! real loaded [`Project`].
//!
//! Per scenario (PRD §Testing): `ymx_lib::load_project` ->
//! `ymx_config::extract_options` with `CliOverrides::default_for_tests()` ->
//! `ymx_test::parse_tests` + `ymx_test::run_tests`. A scenario is a FAIL if
//! loading or option extraction fails unexpectedly, if `parse_tests` errors,
//! or if any test result is not `passed`. The `_test._build_error` key
//! handles expected load/option-time failures (see PRD §Testing). Failures
//! are collected across all scenarios and asserted zero at the end, so a
//! single run reports every failing scenario.
//!
//! The `tests/cases` directory does not exist yet (scenarios land in milestone
//! 1.11); with zero scenarios the harness passes trivially.

use std::path::{Path, PathBuf};

use yaml_rust2::{Yaml, YamlLoader};

use ymx_config::CliOverrides;
use ymx_lib::Diagnostic;
use ymx_lib::Value;
use ymx_test::Expected;

/// Run every scenario under `<workspace root>/tests/cases/rule-NN/<scenario>/`.
///
/// The cases root is two directory levels above this crate
/// (`crates/ymx-test` -> workspace root), resolved at runtime via
/// `CARGO_MANIFEST_DIR` so the harness does not depend on the process cwd.
#[test]
fn run_all_scenarios() {
    let cases = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/cases");
    let Ok(cases) = cases.canonicalize() else {
        return; // no scenarios yet — zero scenarios pass trivially
    };
    let mut failures: Vec<String> = Vec::new();
    for rule_dir in sorted_dirs(&cases) {
        for scenario in sorted_dirs(&rule_dir) {
            run_scenario(&scenario, &mut failures);
        }
    }
    assert!(
        failures.is_empty(),
        "{} scenario failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The sorted subdirectory paths of `dir` (deterministic scenario order).
fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// Returns the expected build error code from `dir/main.yml`'s top-level
/// `_test._build_error` key, or `None` if not present.
fn build_error_code(dir: &Path) -> Option<String> {
    let main_yml = dir.join("main.yml");
    let Ok(contents) = std::fs::read_to_string(&main_yml) else {
        return None;
    };
    let Ok(docs) = YamlLoader::load_from_str(&contents) else {
        return None;
    };
    let Some(Yaml::Hash(top)) = docs.first().cloned() else {
        return None;
    };
    let Some(Yaml::Hash(test_block)) = top.get(&Yaml::String("_test".into())) else {
        return None;
    };
    let Some(Yaml::String(code)) = test_block.get(&Yaml::String("_build_error".into())) else {
        return None;
    };
    Some(code.clone())
}

/// Run one scenario's full pipeline, appending a rendered failure description
/// per problem found.
fn run_scenario(dir: &Path, failures: &mut Vec<String>) {
    let name = dir.display().to_string();
    let build_error = build_error_code(dir);

    let project = match ymx_lib::load_project(dir) {
        Ok(project) => project,
        Err(diags) => {
            if let Some(ref expected_code) = build_error {
                if diags.iter().any(|d| d.code == *expected_code) {
                    return; // expected error — PASS
                }
                failures.push(format!(
                    "{name}: load_project failed with {expected_code} expected, but got:\n{}",
                    render_diags(&diags)
                ));
            } else {
                failures.push(format!(
                    "{name}: load_project failed:\n{}",
                    render_diags(&diags)
                ));
            }
            return;
        }
    };

    let opts = match ymx_config::extract_options(&project, &CliOverrides::default_for_tests()) {
        Ok(opts) => opts,
        Err(diags) => {
            if let Some(ref expected_code) = build_error {
                if diags.iter().any(|d| d.code == *expected_code) {
                    return; // expected error — PASS
                }
                failures.push(format!(
                    "{name}: extract_options failed with {expected_code} expected, but got:\n{}",
                    render_diags(&diags)
                ));
            } else {
                failures.push(format!(
                    "{name}: extract_options failed:\n{}",
                    render_diags(&diags)
                ));
            }
            return;
        }
    };

    // If `_build_error` was set but we reached `parse_tests`, the scenario was
    // supposed to fail at load/extract but didn't — that's a failure.
    if build_error.is_some() {
        failures.push(format!(
            "{name}: _build_error was set but load and extract_options succeeded",
        ));
        return;
    }

    if let Err(diags) = ymx_test::parse_tests(&project) {
        failures.push(format!(
            "{name}: parse_tests failed:\n{}",
            render_diags(&diags)
        ));
        return;
    }

    for result in ymx_test::run_tests(&project, &opts) {
        if !result.passed {
            failures.push(format!(
                "{name}: FAIL on `{}`\n  expected: {}\n  actual: {}",
                result.test.target,
                expected_text(&result.test.expected),
                actual_text(&result.actual),
            ));
        }
    }
}

/// The expected outcome rendered for a failing test.
fn expected_text(expected: &Expected) -> String {
    match expected {
        Expected::Value(v) => value_text(v),
        Expected::Error { code } => format!("diagnostic code {code}"),
    }
}

/// The actual compile result rendered for a failing test.
fn actual_text(actual: &Result<Value, Vec<Diagnostic>>) -> String {
    match actual {
        Ok(v) => value_text(v),
        Err(diags) => render_diags(diags),
    }
}

/// A `Value` rendered as JSON (insertion-ordered), falling back to `Debug`.
fn value_text(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| format!("{v:?}"))
}

/// Diagnostics rendered one per line in the `[code] file:line:col
/// (component): message` format.
fn render_diags(diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| d.render())
        .collect::<Vec<String>>()
        .join("\n")
}
