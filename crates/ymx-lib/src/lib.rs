//! `ymx-lib` — thin façade over `ymx-core` plus the project-loading I/O helper.
//!
//! This is the pipeline's only filesystem entry point: [`load_project`] walks
//! a root directory, parses every `.yml`/`.yaml` document with `ymx-core`'s
//! spanned parser, and assembles the [`Project`] — namespace merge,
//! file-scoped definitions, and raw `_ymx`/`_test` meta values — without
//! interpreting the meta values (that is `ymx-config` / `ymx-test`'s job).
//! Loading is all-or-nothing: any load-time diagnostic (`E001` / `E004` /
//! `E007` / `E015`) fails the whole load with `Err`, so no `Project` is
//! produced for a project that does not load cleanly.
//!
//! `ymx-lib` deliberately contains no `_ymx` / `_test` logic.

use std::fs;
use std::path::{Path, PathBuf};

use ymx_core::diag::{FileId, E001};
use ymx_core::namespace::{extract_document, DefClass};
use ymx_core::parse::parse_document;

pub use ymx_core;
pub use ymx_core::diag::Diagnostic;
pub use ymx_core::ir::Value;
pub use ymx_core::project::{Format, Options, Project};

/// Walks `root` (`.yml`/`.yaml`), parses each document with spans, builds the
/// [`Project`] (namespace merge, duplicate/file-scope/reserved-name checks),
/// and collects raw `_ymx`/`_test` meta values without interpreting them.
/// I/O lives here so `ymx-core` stays I/O-free.
///
/// # Ordering
///
/// Files are loaded in lexicographic path order: the relative paths of all
/// `.yml`/`.yaml` documents under `root` are collected and sorted
/// (component-wise byte lexicographic — deterministic), then each is assigned
/// a [`FileId`] in that order and processed. Root-level files register into
/// the global namespace (`""`); a file under `a/b/` registers into the dotted
/// sub-namespace `"a.b"`. `Project.files` and the raw-meta vectors therefore
/// appear in the same deterministic order.
///
/// # All-or-nothing (invariant #2)
///
/// Any load-time diagnostic fails the entire load: `Err(diags)` is returned
/// and no [`Project`] is produced. All diagnostics across all files are
/// collected before deciding (no short-circuiting on the first error).
/// Every diagnostic carries its resolved host-file path (invariant #5) —
/// `root`-joined, so load-errors render without a `Project`.
///
/// Diagnostics produced here: `E001` (YAML parse error / unsupported YAML
/// feature, including a non-string top-level key and filesystem read
/// failures), `E004` (duplicate name in the same namespace or file scope),
/// `E007` (reserved builtin name), `E015` (leading-`$` meta-key variant).
pub fn load_project(root: &Path) -> Result<Project, Vec<Diagnostic>> {
    let mut project = Project::new();
    project.root = root.to_path_buf();
    let mut diags = Vec::new();

    let mut files = Vec::new();
    walk(root, &mut files, &mut diags);
    files.sort();

    for path in &files {
        let file_id = FileId(project.files.len() as u32);
        project.files.push(path.clone());
        let relative = path.strip_prefix(root).unwrap_or(path);
        let namespace = namespace_path(relative);

        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) => {
                diags.push(io_diagnostic(path, &err));
                continue;
            }
        };
        let node = match parse_document(&contents) {
            Ok(node) => node,
            Err(parse_err) => {
                diags.push(parse_err.into_diagnostic(path.clone()));
                continue;
            }
        };
        let extract = extract_document(file_id, &node);
        for def in extract.defs {
            if let Err(dup) = project.namespaces.register(&namespace, def) {
                diags.push(dup.into_diagnostic(path.clone()));
            }
        }
        for def in extract.file_scoped_defs {
            if let Err(dup) = project.file_scoped.register(file_id, def) {
                diags.push(dup.into_diagnostic(path.clone()));
            }
        }
        for class in extract.rejections {
            match class {
                DefClass::InvalidName(span) => diags.push(Diagnostic {
                    file: Some(path.clone()),
                    line: span.line,
                    col: span.col,
                    component: None,
                    code: E001,
                    message: "invalid top-level name (must match `[$]*[A-Za-z_][A-Za-z0-9_]*`; a non-string key cannot name a component or template)".to_string(),
                }),
                class => {
                    if let Some(diag) = class.into_diagnostic(path.clone()) {
                        diags.push(diag);
                    }
                }
            }
        }
        if let Some(meta) = extract.meta_ymx {
            project.raw_meta_ymx.push((meta.file, meta.value));
        }
        if let Some(meta) = extract.meta_test {
            project.raw_meta_test.push((meta.file, meta.value));
        }
    }

    if diags.is_empty() {
        Ok(project)
    } else {
        Err(diags)
    }
}

/// Recursively collect the `.yml`/`.yaml` document paths under `dir` into
/// `files`, pushing an `E001` diagnostic into `diags` for any directory that
/// cannot be enumerated.
fn walk(dir: &Path, files: &mut Vec<PathBuf>, diags: &mut Vec<Diagnostic>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            diags.push(io_diagnostic(dir, &err));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                diags.push(io_diagnostic(dir, &err));
                continue;
            }
        };
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            walk(&path, files, diags);
        } else if is_document(&path) {
            files.push(path);
        }
    }
}

/// `true` iff `path` ends in `.yml` or `.yaml` (exact lowercase extension).
fn is_document(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("yml") | Some("yaml")
    )
}

/// Dotted namespace path for a document's relative path: the parent
/// directory's components joined with `.`. Root-level documents map to the
/// global namespace `""`.
fn namespace_path(relative: &Path) -> String {
    let parts: Vec<&str> = relative
        .parent()
        .iter()
        .flat_map(|parent| parent.components())
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    parts.join(".")
}

/// An `E001` diagnostic for a filesystem failure at `path` (no source span
/// exists for I/O errors; anchor at 1:1).
fn io_diagnostic(path: &Path, err: &std::io::Error) -> Diagnostic {
    Diagnostic {
        file: Some(path.to_path_buf()),
        line: 1,
        col: 1,
        component: None,
        code: E001,
        message: format!("cannot read `{}`: {err}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use ymx_core::diag::{E001, E004, E007, E015};
    use ymx_core::parse::node_to_value;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique-per-test temp directory under the platform temp dir; removed on
    /// drop (best effort).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ymx_load_test_{}_{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent dirs");
            }
            fs::write(path, contents).expect("write file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Parse `src` with `ymx-core` and strip spans, to build expected raw-meta
    /// values without reaching into `indexmap` from this crate.
    fn value_of(src: &str) -> Value {
        node_to_value(&parse_document(src).expect("parse inline yaml"))
    }

    fn file_id_of(project: &Project, relative: &str) -> FileId {
        let path = project
            .files
            .iter()
            .position(|p| p.ends_with(relative))
            .expect("file loaded") as u32;
        FileId(path)
    }

    #[test]
    fn loads_multi_file_tree_in_lex_order() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("a.yml", "a: 2\n");
        dir.write("subdir/b.yml", "b: 3\n");
        dir.write("subdir/nested/c.yml", "c: 4\n");

        let project = load_project(dir.path()).expect("loads cleanly");
        let expected: Vec<PathBuf> = ["a.yml", "main.yml", "subdir/b.yml", "subdir/nested/c.yml"]
            .iter()
            .map(|rel| dir.path().join(rel))
            .collect();
        assert_eq!(project.files, expected, "lexicographic path order");
        assert_eq!(
            project.root,
            dir.path(),
            "root recorded for entry resolution"
        );

        let a = project.namespaces.get("", "a").expect("a global");
        let main = project.namespaces.get("", "main").expect("main global");
        let b = project.namespaces.get("subdir", "b").expect("b in subdir");
        let c = project
            .namespaces
            .get("subdir.nested", "c")
            .expect("c in subdir.nested");
        assert_eq!(a.file, FileId(0));
        assert_eq!(main.file, FileId(1));
        assert_eq!(b.file, FileId(2));
        assert_eq!(c.file, FileId(3));

        assert!(
            project.namespaces.get("", "b").is_none(),
            "subdir defs are not in the global namespace"
        );
        assert!(
            project.namespaces.get("subdir", "c").is_none(),
            "nested defs are not in the parent sub-namespace"
        );
        assert_eq!(
            project.namespaces.len(),
            3,
            "global + subdir + subdir.nested"
        );
    }

    #[test]
    fn parse_error_diagnostic_carries_resolved_path() {
        let dir = TempDir::new();
        dir.write("bad.yml", "a: 1\n---\nb: 2\n");

        let err = load_project(dir.path()).expect_err("multi-doc stream is E001");
        assert_eq!(err.len(), 1);
        let diag = &err[0];
        assert_eq!(diag.code, E001);
        assert_eq!(
            diag.file.as_deref(),
            Some(dir.path().join("bad.yml").as_path())
        );
        assert!(
            diag.render().contains("bad.yml"),
            "renderable without a Project"
        );
    }

    #[test]
    fn duplicate_across_root_files_is_e004() {
        let dir = TempDir::new();
        dir.write("a.yml", "dup: 1\n");
        dir.write("m.yml", "dup: 2\n");

        let err = load_project(dir.path()).expect_err("duplicate in global namespace");
        assert_eq!(err.len(), 1);
        let diag = &err[0];
        assert_eq!(diag.code, E004);
        assert_eq!(diag.component.as_deref(), Some("dup"));
        assert_eq!(
            diag.file.as_deref(),
            Some(dir.path().join("m.yml").as_path()),
            "the second file in lex order is the duplicate"
        );
        assert_eq!((diag.line, diag.col), (1, 1));
    }

    #[test]
    fn duplicate_within_subdir_is_e004() {
        let dir = TempDir::new();
        dir.write("subdir/x.yml", "same: 1\n");
        dir.write("subdir/y.yml", "same: 2\n");

        let err = load_project(dir.path()).expect_err("duplicate in subdir namespace");
        assert_eq!(err.len(), 1);
        let diag = &err[0];
        assert_eq!(diag.code, E004);
        assert_eq!(diag.component.as_deref(), Some("same"));
        assert_eq!(
            diag.file.as_deref(),
            Some(dir.path().join("subdir/y.yml").as_path())
        );
    }

    #[test]
    fn same_name_across_different_namespaces_is_ok() {
        let dir = TempDir::new();
        dir.write("shared.yml", "shared: 1\n");
        dir.write("subdir/shared.yml", "shared: 2\n");

        let project = load_project(dir.path()).expect("different namespaces do not collide");
        assert_eq!(
            project.namespaces.get("", "shared").unwrap().file,
            FileId(0)
        );
        assert_eq!(
            project.namespaces.get("subdir", "shared").unwrap().file,
            FileId(1)
        );
    }

    #[test]
    fn file_scoped_defs_stay_out_of_namespaces() {
        let dir = TempDir::new();
        dir.write("main.yml", "_x: 1\nmain: 2\n");
        dir.write("other.yml", "_y: 3\nother: 4\n");

        let project = load_project(dir.path()).expect("loads cleanly");
        let main_id = file_id_of(&project, "main.yml");
        let other_id = file_id_of(&project, "other.yml");
        assert_ne!(main_id, other_id);

        assert!(project.namespaces.get("", "_x").is_none());
        assert!(project.namespaces.get("", "_y").is_none());
        assert_eq!(
            project.file_scoped.get(main_id, "_x").unwrap().full_name,
            "_x"
        );
        assert_eq!(
            project.file_scoped.get(other_id, "_y").unwrap().full_name,
            "_y"
        );
        assert!(project.file_scoped.get(main_id, "_y").is_none());
        assert!(project.file_scoped.get(other_id, "_x").is_none());
        assert_eq!(project.file_scoped.total(), 2);
        assert_eq!(project.namespaces.get("", "main").unwrap().file, main_id);
        assert_eq!(project.namespaces.get("", "other").unwrap().file, other_id);
    }

    #[test]
    fn e007_and_e015_rejections_carry_path_and_component() {
        let dir = TempDir::new();
        dir.write("e007.yml", "map: 1\nok: 2\n");

        let err = load_project(dir.path()).expect_err("builtin name is E007");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E007);
        assert_eq!(err[0].component.as_deref(), Some("map"));
        assert_eq!(
            err[0].file.as_deref(),
            Some(dir.path().join("e007.yml").as_path())
        );

        let dir = TempDir::new();
        dir.write("e015.yml", "$_ymx: 1\n");

        let err = load_project(dir.path()).expect_err("meta-key variant is E015");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E015);
        assert_eq!(err[0].component.as_deref(), Some("$_ymx"));
        assert_eq!(
            err[0].file.as_deref(),
            Some(dir.path().join("e015.yml").as_path())
        );
    }

    #[test]
    fn all_rejections_in_one_file_are_collected() {
        let dir = TempDir::new();
        dir.write(
            "bad.yml",
            "map: 1\nreduce: 2\n$_ymx: 3\n$$_test: 4\nok: 5\n",
        );

        let err = load_project(dir.path()).expect_err("rejections never abort early");
        let codes: Vec<&str> = err.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![E007, E007, E015, E015]);
        for diag in &err {
            assert_eq!(
                diag.file.as_deref(),
                Some(dir.path().join("bad.yml").as_path()),
                "every rejection carries the host-file path"
            );
        }
    }

    #[test]
    fn raw_meta_values_land_in_lex_order() {
        let dir = TempDir::new();
        dir.write("a.yml", "_ymx:\n  v: 1\n_test:\n  t: a\na: 0\n");
        dir.write("m.yml", "_ymx:\n  v: 2\nm: 0\n");
        dir.write("subdir/z.yml", "_test:\n  t: b\nz: 0\n");

        let project = load_project(dir.path()).expect("loads cleanly");
        let a_id = file_id_of(&project, "a.yml");
        let m_id = file_id_of(&project, "m.yml");
        let z_id = file_id_of(&project, "z.yml");
        assert_eq!(a_id, FileId(0));
        assert_eq!(m_id, FileId(1));
        assert_eq!(z_id, FileId(2));

        assert_eq!(
            project.raw_meta_ymx,
            vec![(a_id, value_of("v: 1\n")), (m_id, value_of("v: 2\n")),],
            "_ymx values in lexicographic load order"
        );
        assert_eq!(
            project.raw_meta_test,
            vec![(a_id, value_of("t: a\n")), (z_id, value_of("t: b\n")),],
            "_test values in lexicographic load order"
        );
        assert!(!project.has_no_ymx());
        assert!(!project.has_no_test());
    }

    #[test]
    fn non_document_files_are_ignored() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("notes.txt", "not yaml\n");
        dir.write("README.md", "# readme\n");
        dir.write("subdir/data.yaml", "data: 2\n");
        dir.write("subdir/data.txt", "ignored\n");

        let project = load_project(dir.path()).expect("loads cleanly");
        let expected: Vec<PathBuf> = ["main.yml", "subdir/data.yaml"]
            .iter()
            .map(|rel| dir.path().join(rel))
            .collect();
        assert_eq!(project.files, expected, ".txt/.md are not loaded, .yaml is");
        assert!(project.namespaces.get("", "main").is_some());
        assert!(project.namespaces.get("subdir", "data").is_some());
    }

    #[test]
    fn empty_root_yields_empty_project() {
        let dir = TempDir::new();
        let project = load_project(dir.path()).expect("empty dir loads");
        assert!(project.files.is_empty());
        assert!(project.namespaces.is_empty());
        assert!(project.has_no_ymx());
        assert!(project.has_no_test());
    }

    #[test]
    fn missing_root_is_e001_with_path() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope");
        let err = load_project(&missing).expect_err("missing root cannot load");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E001);
        assert_eq!(err[0].file.as_deref(), Some(missing.as_path()));
    }

    #[test]
    fn non_string_top_level_key_is_e001() {
        let dir = TempDir::new();
        dir.write("bad.yml", "0: a\nmain: 1\n");

        let err = load_project(dir.path()).expect_err("non-string key is invalid");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E001);
        assert_eq!(
            err[0].file.as_deref(),
            Some(dir.path().join("bad.yml").as_path())
        );
        assert_eq!((err[0].line, err[0].col), (1, 1));
    }

    #[test]
    fn meta_values_are_uninterpreted_verbatim() {
        let dir = TempDir::new();
        dir.write("main.yml", "_ymx: 5\n_test:\n  - 1\n  - 2\nmain: 0\n");

        let project = load_project(dir.path()).expect("scalar and array meta load verbatim");
        assert_eq!(project.raw_meta_ymx[0].1, Value::Int(5));
        assert_eq!(
            project.raw_meta_test[0].1,
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }
}
