//! Entry-path resolution (invariant #1) — and, from milestone 1.6, the
//! rule-1–16 resolver.
//!
//! The **entry path** is a file-path address `<folder.path>.<file>.<component>`
//! (e.g. `main.main` = root folder + `main.yml` + component `main`; `a.b.c` =
//! folder `a` + `b.yml` + component `c`). It is **not** a namespace dotted
//! path: `from: subdir.comp` and math `subdir.comp(...)` address namespaces,
//! while the entry pinpoints one file (the front-matter source) plus one
//! component for compilation. [`resolve_entry`] is pure — no I/O — because
//! everything it needs already lives in [`Project`] (root, files, stores).
//!
//! `E009` (options stage) covers: malformed entry paths (fewer than two
//! segments, empty segments, separator-bearing segments), a missing entry
//! file, an ambiguous `.yml`/`.yaml` stem, and a component not defined in the
//! entry file — including names that can never be components (builtins, meta
//! keys, invalid identifiers).

use std::path::{Path, PathBuf};

use crate::diag::{Diagnostic, FileId, Span, E009};
use crate::namespace::{classify, DefClass};
use crate::project::Project;

/// Resolve the entry path `<folder.path>.<file>.<component>` against an
/// already-loaded [`Project`].
///
/// Returns `(front-matter FileId, namespace, component)`:
/// * the [`FileId`] of the entry document (the front-matter source — its raw
///   `_ymx`/`_test` meta is what `ymx-config` / the CLI consume);
/// * the namespace the component lives in — the dotted folder path of the
///   entry (empty string for root-level files);
/// * the component name as written in the entry path.
///
/// Segment grammar: the penultimate segment is the file stem (extensionless);
/// all segments before it form the folder path (dotted in the entry, joined
/// with `/` on disk); the last segment is the component name. A component is
/// considered "defined in the entry file" if it is a non-`_` definition whose
/// hosting file is the entry document, or a file-scoped `_`-prefixed
/// definition stored for that document (file-scope restricts *references*, not
/// entry pinning — `--entry main._x` compiling `main.yml`'s `_x` is coherent).
///
/// `E009` failures: fewer than two segments; any empty or separator-bearing
/// segment; no `.<folder>/<stem>.yml` **and** no `.<folder>/<stem>.yaml`
/// (missing file — no `file` slot, the attempted path is in the message);
/// both extensions present (ambiguous stem); or the component not defined in
/// the entry file (`file` attached — the document exists). `E009` carries
/// `file: None` only when no loaded document is implicated (invariant #5).
pub fn resolve_entry<'a>(
    project: &Project,
    entry: &'a str,
) -> Result<(FileId, String, &'a str), Diagnostic> {
    let segments: Vec<&str> = entry.split('.').collect();
    if segments.len() < 2 {
        return Err(malformed(
            entry,
            "expected at least two segments (`<folder.path>.<file>.<component>`)",
        ));
    }
    for segment in &segments {
        if segment.is_empty() || segment.contains('/') || segment.contains('\\') {
            return Err(malformed(
                entry,
                "segments must be non-empty dotted-path parts (`<folder.path>.<file>.<component>`)",
            ));
        }
    }
    let folder = &segments[..segments.len() - 2];
    let stem = segments[segments.len() - 2];
    let component = segments[segments.len() - 1];
    let namespace = folder.join(".");

    let mut rel_dir = PathBuf::new();
    for segment in folder {
        rel_dir.push(segment);
    }

    let mut candidates: Vec<FileId> = Vec::new();
    for (idx, path) in project.files.iter().enumerate() {
        let Ok(relative) = path.strip_prefix(&project.root) else {
            continue;
        };
        for ext in ["yml", "yaml"] {
            if relative == rel_dir.join(stem).with_extension(ext) {
                candidates.push(FileId(idx as u32));
            }
        }
    }
    let file_id = match candidates.len() {
        0 => {
            return Err(Diagnostic {
                file: None,
                line: 1,
                col: 1,
                component: None,
                code: E009,
                message: format!(
                    "entry file not found: no `{}` under `{}` (entry `{entry}`)",
                    rel_dir.join(stem).with_extension("yml").display(),
                    project.root.display(),
                ),
            });
        }
        1 => candidates[0],
        2 => {
            return Err(Diagnostic {
                file: None,
                line: 1,
                col: 1,
                component: None,
                code: E009,
                message: format!(
                    "ambiguous entry `{entry}`: both `{}` and `{}` exist",
                    rel_dir.join(stem).with_extension("yml").display(),
                    rel_dir.join(stem).with_extension("yaml").display(),
                ),
            });
        }
        _ => unreachable!("at most one file per extension per stem"),
    };

    let file_path = project.files[file_id.0 as usize].clone();
    match classify(component, Span { line: 1, col: 1 }) {
        DefClass::Component(meta) if meta.file_scoped => {
            if project.file_scoped.get(file_id, component).is_some() {
                Ok((file_id, namespace, component))
            } else {
                Err(component_missing(entry, component, &file_path))
            }
        }
        DefClass::Component(_) => {
            let defined_in_entry_file = project
                .namespaces
                .get(&namespace, component)
                .map(|def| def.file == file_id)
                .unwrap_or(false);
            if defined_in_entry_file {
                Ok((file_id, namespace, component))
            } else {
                Err(component_missing(entry, component, &file_path))
            }
        }
        _ => Err(Diagnostic {
            file: Some(file_path),
            line: 1,
            col: 1,
            component: Some(component.to_string()),
            code: E009,
            message: format!(
                "`{component}` cannot be an entry component: it is not a component or template name (entry `{entry}`)"
            ),
        }),
    }
}

/// A malformed entry path: no document implicated, so no `file` slot.
fn malformed(entry: &str, detail: &str) -> Diagnostic {
    Diagnostic {
        file: None,
        line: 1,
        col: 1,
        component: None,
        code: E009,
        message: format!("invalid entry path `{entry}`: {detail}"),
    }
}

/// The entry document exists but does not define the component.
fn component_missing(entry: &str, component: &str, file_path: &Path) -> Diagnostic {
    Diagnostic {
        file: Some(file_path.to_path_buf()),
        line: 1,
        col: 1,
        component: Some(component.to_string()),
        code: E009,
        message: format!(
            "component `{component}` is not defined in `{}` (entry `{entry}`)",
            file_path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::namespace::Definition;
    use crate::parse::Node;

    const SPAN: Span = Span { line: 1, col: 1 };

    fn def(file: u32, name: &str) -> Definition {
        Definition {
            file: FileId(file),
            full_name: name.to_string(),
            span: SPAN,
            body: Node::Int(1, SPAN),
        }
    }

    /// Project rooted at `/proj`:
    /// * `main.yml`      (FileId 0): `main`, `$box`, `_x`
    /// * `a/b.yml`       (FileId 1): `x`, `c`
    /// * `a/other.yml`   (FileId 2): `y`
    fn project() -> Project {
        let mut p = Project::new();
        p.root = PathBuf::from("/proj");
        p.files = vec![
            PathBuf::from("/proj/main.yml"),
            PathBuf::from("/proj/a/b.yml"),
            PathBuf::from("/proj/a/other.yml"),
        ];
        p.namespaces.register("", def(0, "main")).unwrap();
        p.namespaces.register("", def(0, "$box")).unwrap();
        p.namespaces.register("a", def(1, "x")).unwrap();
        p.namespaces.register("a", def(1, "c")).unwrap();
        p.namespaces.register("a", def(2, "y")).unwrap();
        p.file_scoped.register(FileId(0), def(0, "_x")).unwrap();
        p
    }

    #[test]
    fn entry_resolves_folder_file_component() {
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "a.b.c").unwrap();
        assert_eq!(file_id, FileId(1));
        assert_eq!(namespace, "a");
        assert_eq!(component, "c");
    }

    #[test]
    fn default_entry_main_main_resolves_root_file() {
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "main.main").unwrap();
        assert_eq!(file_id, FileId(0));
        assert_eq!(namespace, "");
        assert_eq!(component, "main");
    }

    #[test]
    fn template_component_is_reachable_via_entry() {
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "main.$box").unwrap();
        assert_eq!(file_id, FileId(0));
        assert_eq!(namespace, "");
        assert_eq!(component, "$box");
    }

    #[test]
    fn file_scoped_component_is_reachable_via_entry() {
        // File-scope restricts cross-document *references* (E005, 1.6), not
        // entry pinning: `--entry main._x` compiles main.yml's `_x`.
        let p = project();
        let (file_id, namespace, component) = resolve_entry(&p, "main._x").unwrap();
        assert_eq!(file_id, FileId(0));
        assert_eq!(namespace, "");
        assert_eq!(component, "_x");
    }

    #[test]
    fn one_segment_is_e009() {
        let p = project();
        let err = resolve_entry(&p, "main").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(err.file, None, "no document implicated by a malformed path");
        assert!(err.message.contains("main"));
    }

    #[test]
    fn empty_and_separator_segments_are_e009() {
        let p = project();
        for entry in ["a..c", ".a.b", "a.b.", "a/b.c", "a\\b.c"] {
            let err = resolve_entry(&p, entry).unwrap_err();
            assert_eq!(err.code, E009, "{entry}");
            assert_eq!(err.file, None, "{entry}");
        }
    }

    #[test]
    fn missing_file_is_e009_with_attempted_path() {
        let p = project();
        let err = resolve_entry(&p, "a.missing.c").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(err.file, None, "the file is not loaded; no FileId exists");
        assert!(
            err.message.contains("a/missing.yml"),
            "message renders the attempted path: {}",
            err.message
        );
    }

    #[test]
    fn ambiguous_stem_is_e009() {
        let mut p = project();
        p.files.push(PathBuf::from("/proj/a/b.yaml"));
        let err = resolve_entry(&p, "a.b.c").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(err.file, None);
        assert!(err.message.contains("b.yml"), "{}", err.message);
        assert!(err.message.contains("b.yaml"), "{}", err.message);
    }

    #[test]
    fn component_not_defined_in_entry_file_is_e009() {
        let p = project();
        // `y` exists in namespace `a` but is defined by a/other.yml, not a/b.yml.
        let err = resolve_entry(&p, "a.b.y").unwrap_err();
        assert_eq!(err.code, E009);
        assert_eq!(
            err.file.as_deref(),
            Some(Path::new("/proj/a/b.yml")),
            "the entry document exists, so it is attached"
        );
        assert_eq!(err.component.as_deref(), Some("y"));
        assert!(err.message.contains("a/b.yml"), "{}", err.message);

        // The same name via its actual file resolves fine.
        let (file_id, namespace, component) = resolve_entry(&p, "a.other.y").unwrap();
        assert_eq!(file_id, FileId(2));
        assert_eq!(namespace, "a");
        assert_eq!(component, "y");
    }

    #[test]
    fn meta_and_builtin_names_cannot_be_entry_components() {
        let p = project();
        for entry in ["main._ymx", "main._test", "main.map", "main.1x"] {
            let err = resolve_entry(&p, entry).unwrap_err();
            assert_eq!(err.code, E009, "{entry}");
            assert_eq!(
                err.file.as_deref(),
                Some(Path::new("/proj/main.yml")),
                "{entry}: the document exists"
            );
        }
    }
}
