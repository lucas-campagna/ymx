//! Diagnostics for the YMX compiler pipeline.
//!
//! [`Diagnostic`] carries its resolved host-file path (`file`) so that
//! load-time diagnostics — which are emitted before a
//! [`Project`](crate::project::Project) exists under the all-or-nothing
//! `load_project` — still render. Load-time codes (`E001`, `E004`, `E007`,
//! `E015`) are therefore **not** `_test`-driveable; options/compile codes are.
//! Render format (PRD): `[code] file:line:col (component): message`.
//!
//! # Stable error codes
//!
//! | Code   | Stage   | Diagnostic |
//! |--------|---------|------------|
//! | `E001` | load    | YAML parse error / unsupported YAML feature |
//! | `E002` | compile | Unknown component reference |
//! | `E003` | compile | Missing required argument |
//! | `E004` | load    | Duplicate component name in the same namespace |
//! | `E005` | compile | File-scope violation |
//! | `E006` | compile | Ambiguous shortcut |
//! | `E007` | load    | Reserved builtin name (`map` / `reduce` / `merge`) |
//! | `E008` | compile | Max-depth exceeded |
//! | `E009` | options | Entry not found |
//! | `E010` | both    | Invalid syntax |
//! | `E011` | compile | Math error / builtin argument type error |
//! | `E012` | compile | Positional arg after named arg |
//! | `E013` | compile | Array/object literal as a direct call arg |
//! | `E015` | load    | Meta-key reserved name |
//! | `E016` | compile | Shell execution error |
//! | `E017` | compile | No array/object as direct tag content |
//! | `E018` | compile | IPC error (no host / spawn / timeout / protocol / hook) |
//!
//! `E014` is intentionally absent — `E003` (missing required argument) covers
//! that case.

use std::path::PathBuf;

pub const E001: &str = "E001";
pub const E002: &str = "E002";
pub const E003: &str = "E003";
pub const E004: &str = "E004";
pub const E005: &str = "E005";
pub const E006: &str = "E006";
pub const E007: &str = "E007";
pub const E008: &str = "E008";
pub const E009: &str = "E009";
pub const E010: &str = "E010";
pub const E011: &str = "E011";
pub const E012: &str = "E012";
pub const E013: &str = "E013";
pub const E015: &str = "E015";
pub const E016: &str = "E016";
pub const E017: &str = "E017";
pub const E018: &str = "E018";

/// Index into [`Project::files`](crate::project::Project::files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FileId(pub u32);

/// 1-based source span anchor (line, column) for a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

/// A structured YMX diagnostic. `file` is the resolved host-file path —
/// `None` only when no file context exists. It is resolved at creation so
/// load-time diagnostics (no `Project` to resolve against) still render.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub file: Option<PathBuf>,
    pub line: u32,
    pub col: u32,
    pub component: Option<String>,
    pub code: &'static str,
    pub message: String,
}

impl Diagnostic {
    /// Render as `[code] file:line:col (component): message`.
    ///
    /// A `None` `file` renders as `<?>` so the colon positions stay aligned;
    /// a `None` `component` likewise renders as `<?>`.
    pub fn render(&self) -> String {
        let file = self
            .file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<?>".to_string());
        let component = self.component.clone().unwrap_or_else(|| "<?>".to_string());
        format!(
            "[{}] {}:{}:{} ({}): {}",
            self.code, file, self.line, self.col, component, self.message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn render_with_file_and_component() {
        let d = Diagnostic {
            file: Some(PathBuf::from("path/to/file.yml")),
            line: 3,
            col: 7,
            component: Some("main".to_string()),
            code: E001,
            message: "Boom!".to_string(),
        };
        assert_eq!(d.render(), "[E001] path/to/file.yml:3:7 (main): Boom!");
    }

    #[test]
    fn render_without_file_uses_placeholder() {
        let d = Diagnostic {
            file: None,
            line: 1,
            col: 5,
            component: Some("main".to_string()),
            code: E010,
            message: "nope".to_string(),
        };
        assert_eq!(d.render(), "[E010] <?>:1:5 (main): nope");
    }

    #[test]
    fn render_without_component_uses_placeholder() {
        let d = Diagnostic {
            file: None,
            line: 2,
            col: 1,
            component: None,
            code: E008,
            message: "deep".to_string(),
        };
        assert_eq!(d.render(), "[E008] <?>:2:1 (<?>): deep");
    }

    #[test]
    fn error_code_constants_are_self_named() {
        assert_eq!(E001, "E001");
        assert_eq!(E002, "E002");
        assert_eq!(E003, "E003");
        assert_eq!(E004, "E004");
        assert_eq!(E005, "E005");
        assert_eq!(E006, "E006");
        assert_eq!(E007, "E007");
        assert_eq!(E008, "E008");
        assert_eq!(E009, "E009");
        assert_eq!(E010, "E010");
        assert_eq!(E011, "E011");
        assert_eq!(E012, "E012");
        assert_eq!(E013, "E013");
        assert_eq!(E015, "E015");
        assert_eq!(E016, "E016");
        assert_eq!(E017, "E017");
        assert_eq!(E018, "E018");
    }
}
