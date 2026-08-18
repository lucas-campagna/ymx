//! Enhanced diagnostic rendering with actionable guidance (milestone 1.23).
//!
//! Wraps [`ymx_lib::Diagnostic`] rendering with context-specific hints
//! based on error codes, helping users understand and fix common issues.

use ymx_lib::Diagnostic;

/// Render a diagnostic with optional actionable guidance appended.
/// Returns the full rendered string suitable for stderr output.
pub fn render_with_guidance(diag: &Diagnostic) -> String {
    let base = diag.render();
    let guidance = guidance_for(diag);
    match guidance {
        Some(hint) => format!("{}\n  Hint: {}", base, hint),
        None => base,
    }
}

/// Return an actionable hint for a diagnostic, or `None` if no specific
/// guidance is available.
fn guidance_for(diag: &Diagnostic) -> Option<String> {
    match diag.code {
        "E001" => some_yaml_parse_hint(&diag.message),
        "E002" => Some("check that the component name is defined in the file".to_string()),
        "E004" => Some("each component and template must have a unique name within its namespace".to_string()),
        "E007" => Some("`map`, `reduce`, and `merge` are reserved builtin names; use different names for your components".to_string()),
        "E008" => Some("use `--max-depth <n>` to increase the recursion limit (default: 256)".to_string()),
        "E009" => some_entry_hint(&diag.message),
        "E010" => Some("review the syntax error details above; common issues include malformed template calls or invalid key names".to_string()),
        "E011" => Some("check builtin argument types in the message above; math operations require numbers".to_string()),
        "E012" => Some("all positional arguments must come before any named arguments".to_string()),
        "E013" => Some("array or object literals cannot be used directly as call arguments; assign to a variable first".to_string()),
        _ => None,
    }
}

/// YAML parse error specific hints based on common patterns.
fn some_yaml_parse_hint(msg: &str) -> Option<String> {
    if msg.contains("unexpected end") || msg.contains("unexpected token") {
        Some("check for unclosed brackets, quotes, or braces".to_string())
    } else if msg.contains("duplicate key") {
        Some("YAML does not allow duplicate keys in mappings".to_string())
    } else if msg.contains("invalid syntax") || msg.contains("unexpected `---`") {
        Some(
            "YAML multi-document streams (---) are not supported; use a single document"
                .to_string(),
        )
    } else {
        Some("validate the YAML syntax with an external parser".to_string())
    }
}

/// Entry error specific hints based on the error message content.
fn some_entry_hint(msg: &str) -> Option<String> {
    if msg.contains("file") || msg.contains("not found") || msg.contains("missing") {
        Some("the entry path must point to an existing .yml or .yaml file".to_string())
    } else if msg.contains("ambiguous") {
        Some(
            "both .yml and .yaml variants exist for this stem; use the one that matches your file"
                .to_string(),
        )
    } else if msg.contains("component") {
        Some("specify an existing component with --entry <name> (default: main)".to_string())
    } else {
        Some("check the entry path format: use the file stem (not full path) with --entry <component>".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn diag(code: &'static str, message: &str) -> Diagnostic {
        Diagnostic {
            file: Some(PathBuf::from("test.yml")),
            line: 1,
            col: 1,
            component: Some("test".to_string()),
            code,
            message: message.to_string(),
        }
    }

    #[test]
    fn render_with_guidance_adds_hint_for_e001() {
        let d = diag("E001", "unexpected end of file");
        let rendered = render_with_guidance(&d);
        assert!(rendered.contains("Hint:"), "should add hint");
        assert!(rendered.contains("unclosed"), "should have specific hint");
    }

    #[test]
    fn render_with_guidance_adds_hint_for_e002() {
        let d = diag("E002", "unknown component foo");
        let rendered = render_with_guidance(&d);
        assert!(rendered.contains("Hint:"), "should add hint");
    }

    #[test]
    fn render_with_guidance_adds_hint_for_e009_ambiguous() {
        let d = diag("E009", "ambiguous entry: both main.yml and main.yaml exist");
        let rendered = render_with_guidance(&d);
        assert!(
            rendered.contains("both .yml and .yaml"),
            "should suggest using existing file"
        );
    }

    #[test]
    fn render_with_guidance_adds_hint_for_e009_missing_file() {
        let d = diag("E009", "file not found: foo.yml");
        let rendered = render_with_guidance(&d);
        assert!(
            rendered.contains("entry path must point"),
            "should suggest entry path format"
        );
    }

    #[test]
    fn render_with_guidance_no_hint_for_unknown_code() {
        let d = diag("E999", "some unknown error");
        let rendered = render_with_guidance(&d);
        assert!(
            !rendered.contains("Hint:"),
            "should not add hint for unknown codes"
        );
    }

    #[test]
    fn render_with_guidance_preserves_base_rendering() {
        let d = diag("E001", "parse error");
        let rendered = render_with_guidance(&d);
        assert!(rendered.contains("[E001]"), "should include code");
        assert!(rendered.contains("test.yml"), "should include file");
    }
}
