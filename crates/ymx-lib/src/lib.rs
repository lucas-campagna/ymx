//! `ymx-lib` — thin façade over `ymx-core` plus the project-loading I/O helper.
//!
//! This is the pipeline's only filesystem entry point: [`load_project`] resolves
//! the `_use` graph from an entry file, parses each document with `ymx-core`'s
//! spanned parser, and assembles the [`Project`] — namespace merge,
//! file-scoped definitions, and raw `_ymx`/`_test` meta values — without
//! interpreting the meta values (that is `ymx-config` / `ymx-test`'s job).
//! Loading is all-or-nothing: any load-time diagnostic (`E001` / `E004` /
//! `E007` / `E015`) fails the whole load with `Err`, so no `Project` is
//! produced for a project that does not load cleanly.
//!
//! `ymx-lib` deliberately contains no `_ymx` / `_test` / `_use` logic.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ymx_core::diag::{FileId, E001, E002, E005, E009};
use ymx_core::namespace::{extract_document, DefClass};
use ymx_core::parse::parse_document;

pub use ymx_core;
pub use ymx_core::diag::Diagnostic;
pub use ymx_core::ir::Value;
pub use ymx_core::project::{Format, Options, Project};

/// The three forms of `_use` values (parsed from `MetaValue.value`).
#[derive(Debug, Clone)]
enum RawUse {
    /// `_use: *` → recursive wildcard walk of the entry's directory
    WildcardAll,
    /// `_use: {"*": "foo"}` → import all public components from `foo.yml`
    WildcardFile(String),
    /// `_use: {x: "foo.bar", ...}` → named imports
    NamedImports(Vec<(String, String, String)>), // (alias, file_path, component)
}

/// Parse a `Value` (from `node_to_value`) into a `RawUse`.
fn parse_raw_use(value: &ymx_core::ir::Value) -> Option<RawUse> {
    match value {
        // Bare `*` → wildcard all
        ymx_core::ir::Value::String(s) if s == "*" => Some(RawUse::WildcardAll),
        // Object form
        ymx_core::ir::Value::Object(m) if !m.is_empty() => {
            // Check for wildcard: {"*": "foo"}
            if let Some(ymx_core::ir::Value::String(f)) = m.get("*") {
                // Wildcard file import: {"*": "foo"}
                return Some(RawUse::WildcardFile(f.clone()));
            }
            // Named imports: {alias: "file.component", ...}
            let mut named = Vec::new();
            for (alias, v) in m.iter() {
                if alias == "*" {
                    // `*` key with non-string value — skip and treat as named
                    continue;
                }
                if let ymx_core::ir::Value::String(rhs) = v {
                    // RHS is "file.component" — file may contain dots (e.g. "src.utils.foo")
                    let parts: Vec<&str> = rhs.split('.').collect();
                    if parts.len() >= 2 {
                        let component = parts.last().unwrap();
                        let file_path = parts[..parts.len() - 1].join(".");
                        named.push((alias.clone(), file_path, component.to_string()));
                    } else {
                        return None; // invalid RHS format
                    }
                } else {
                    return None;
                }
            }
            if named.is_empty() {
                None
            } else {
                Some(RawUse::NamedImports(named))
            }
        }
        _ => None,
    }
}

/// Resolve a wildcard file stem where dots are path separators (e.g., "subdir.lib" → subdir/lib.yml).
fn resolve_wildcard_file_stem(stem: &str, dir: &Path) -> Result<PathBuf, Diagnostic> {
    let with_sep = stem.replace('.', "/");
    let yml_path = dir.join(&with_sep).with_extension("yml");
    let yaml_path = dir.join(&with_sep).with_extension("yaml");

    let yml_exists = yml_path.exists();
    let yaml_exists = yaml_path.exists();

    if yml_exists && yaml_exists {
        return Err(Diagnostic {
            file: Some(dir.to_path_buf()),
            line: 1,
            col: 1,
            component: None,
            code: E009,
            message: format!(
                "ambiguous file stem `{}`: both `{}.yml` and `{}.yaml` exist in `{}`",
                stem, stem, stem, dir.display()
            ),
        });
    }

    if yml_exists {
        Ok(yml_path)
    } else if yaml_exists {
        Ok(yaml_path)
    } else {
        Err(Diagnostic {
            file: Some(dir.to_path_buf()),
            line: 1,
            col: 1,
            component: None,
            code: E009,
            message: format!(
                "file stem `{}` does not resolve to a `.yml` or `.yaml` file in `{}`",
                stem,
                dir.display()
            ),
        })
    }
}

/// Resolve a file stem to an actual file path under `dir`. Returns the resolved file path,
/// or an error diagnostic. E009 if both .yml and .yaml exist, or neither exists.
fn resolve_file_stem(stem: &str, dir: &Path) -> Result<PathBuf, Diagnostic> {
    let yml_path = dir.join(format!("{}.yml", stem));
    let yaml_path = dir.join(format!("{}.yaml", stem));

    let yml_exists = yml_path.exists();
    let yaml_exists = yaml_path.exists();

    if yml_exists && yaml_exists {
        return Err(Diagnostic {
            file: Some(dir.to_path_buf()),
            line: 1,
            col: 1,
            component: None,
            code: E009,
            message: format!(
                "ambiguous file stem `{}`: both `{}.yml` and `{}.yaml` exist in `{}`",
                stem, stem, stem, dir.display()
            ),
        });
    }

    if yml_exists {
        Ok(yml_path)
    } else if yaml_exists {
        Ok(yaml_path)
    } else {
        Err(Diagnostic {
            file: Some(dir.to_path_buf()),
            line: 1,
            col: 1,
            component: None,
            code: E009,
            message: format!(
                "file stem `{}` does not resolve to a `.yml` or `.yaml` file in `{}`",
                stem,
                dir.display()
            ),
        })
    }
}

/// Build the full import graph starting from `entry_file`. The entry file must exist.
/// Returns (ordered_file_paths, diags) where ordered_file_paths is topologically sorted.
/// `diags` collects E001 (cycle), E009 (missing file).
fn resolve_use_graph(entry_file: &Path) -> Result<Vec<PathBuf>, Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let project_root = entry_file
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    let mut result: Vec<PathBuf> = Vec::new();

    // DFS from entry file; is_entry=true only for the initial call
    fn dfs(
        file_path: &Path,
        project_root: &Path,
        is_entry: bool,
        visited: &mut HashSet<PathBuf>,
        stack: &mut Vec<PathBuf>,
        result: &mut Vec<PathBuf>,
        diags: &mut Vec<Diagnostic>,
    ) {
        // Canonicalize path
        let canonical = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());

        // Cycle detection
        if stack.contains(&canonical) {
            diags.push(Diagnostic {
                file: Some(file_path.to_path_buf()),
                line: 1,
                col: 1,
                component: None,
                code: E001,
                message: format!(
                    "cycle detected in `_use` graph: `{}` is already on the import stack",
                    file_path.display()
                ),
            });
            return;
        }

        if visited.contains(&canonical) {
            // Already processed (via a different path)
            return;
        }

        visited.insert(canonical.clone());
        stack.push(canonical.clone());

        // Parse the file to get its `_use` directive
        let contents = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(err) => {
                diags.push(Diagnostic {
                    file: Some(file_path.to_path_buf()),
                    line: 1,
                    col: 1,
                    component: None,
                    code: E001,
                    message: format!("cannot read `{}`: {}", file_path.display(), err),
                });
                stack.pop();
                return;
            }
        };

        let node = match parse_document(&contents) {
            Ok(n) => n,
            Err(parse_err) => {
                diags.push(parse_err.into_diagnostic(file_path.to_path_buf()));
                stack.pop();
                return;
            }
        };

        let file_id = FileId(0); // temporary; not used for _use parsing
        let extract = extract_document(file_id, &node);
        let raw_use = extract.meta_use.and_then(|mv| parse_raw_use(&mv.value));

        // Process the _use directive
        // None means: for entry file, do wildcard walk (backward compat); for imports, do nothing
        if let Some(RawUse::WildcardAll) = raw_use {
            // Explicit wildcard: walk the file's directory
            let dir = file_path.parent().unwrap_or(project_root);
            let mut files = Vec::new();
            walk_only(dir, &mut files);
            files.sort();
            // Filter out the current file to avoid duplicate processing
            files.retain(|f| f != file_path);
            for f in files {
                dfs(&f, project_root, false, visited, stack, result, diags);
            }
        } else if raw_use.is_none() && is_entry {
            // Entry file with no _use: backward compat as _use: * (walk)
            let dir = file_path.parent().unwrap_or(project_root);
            let mut files = Vec::new();
            walk_only(dir, &mut files);
            files.sort();
            // Filter out the current file to avoid duplicate processing
            files.retain(|f| f != file_path);
            for f in files {
                dfs(&f, project_root, false, visited, stack, result, diags);
            }
        } else if let Some(RawUse::WildcardFile(stem)) = raw_use {
            // Import all from a specific file (dots are path separators)
            match resolve_wildcard_file_stem(&stem, project_root) {
                Ok(target) => {
                    dfs(&target, project_root, false, visited, stack, result, diags);
                }
                Err(e) => {
                    diags.push(e);
                }
            }
        } else if let Some(RawUse::NamedImports(imports)) = raw_use {
            // Named imports
            for (_alias, file_path_str, _component) in imports {
                match resolve_file_stem(&file_path_str, project_root) {
                    Ok(target) => {
                        dfs(&target, project_root, false, visited, stack, result, diags);
                    }
                    Err(e) => {
                        diags.push(e);
                    }
                }
            }
        }
        // For None (non-entry): do nothing - only the file itself is loaded

        // After processing _use, add the current file to result
        // (post-order: dependencies first, then this file)
        result.push(file_path.to_path_buf());
        stack.pop();
    }

    // Verify entry file exists
    if !entry_file.exists() {
        return Err(vec![Diagnostic {
            file: Some(entry_file.to_path_buf()),
            line: 1,
            col: 1,
            component: None,
            code: E001,
            message: format!("entry file `{}` does not exist", entry_file.display()),
        }]);
    }

    dfs(
        entry_file,
        &project_root,
        true, // is_entry
        &mut visited,
        &mut stack,
        &mut result,
        &mut diags,
    );

    // Sort result lexicographically for deterministic ordering
    result.sort();

    if diags.is_empty() {
        Ok(result)
    } else {
        Err(diags)
    }
}

/// Walk `dir` collecting all `.yml`/`.yaml` file paths (not recursive into subdirs
/// that have no YAML files directly).
fn walk_only(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            // Skip .git and hidden directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == ".git" || name.starts_with('.') {
                    continue;
                }
            }
            // Check if subdir has any YAML files directly
            let mut has_yaml = false;
            if let Ok(sub_entries) = fs::read_dir(&path) {
                for sub_entry in sub_entries.flatten() {
                    if let Some(ext) = sub_entry.path().extension() {
                        if ext == "yml" || ext == "yaml" {
                            has_yaml = true;
                            break;
                        }
                    }
                }
            }
            if has_yaml {
                // Recurse into subdir
                walk_only(&path, files);
            }
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

/// Walks `root` (`.yml`/`.yaml`), parses each document with spans, builds the
/// [`Project`] (namespace merge, duplicate/file-scope/reserved-name checks),
/// and collects raw `_ymx`/`_test` meta values without interpreting them.
/// I/O lives here so `ymx-core` stays I/O-free.
///
/// `root` is the **entry file path** (not a directory). The project root is
/// derived as `root.parent()`. If `root` is a directory, it is searched for
/// `main.yml` or `main.yaml` as the entry file.
///
/// `_use` directive handling:
/// - `_use: *` → recursive wildcard walk of the entry's directory
/// - `_use: {"*": "file"}` → import all public components from `file.yml`
/// - `_use: {x: "file.component"}` → import `component` from `file.yml` as `x`
/// - If no `_use` key is present, behaves as `_use: *` (backward compat)
///
/// All imported components land in the **global namespace**. File-scoped
/// components (`_`-prefixed) cannot be imported (E005).
///
/// # All-or-nothing (invariant #2)
///
/// Any load-time diagnostic fails the entire load: `Err(diags)` is returned
/// and no [`Project`] is produced. All diagnostics across all files are
/// collected before deciding (no short-circuiting on the first error).
///
/// Diagnostics produced here: `E001` (YAML parse error / unsupported YAML
/// feature, filesystem read failures, cycles), `E002` (unknown imported
/// component), `E004` (duplicate name in global namespace), `E005`
/// (imported a file-scoped component), `E007` (reserved builtin name),
/// `E009` (target file not found or ambiguous), `E015` (leading-`$` meta-key variant).
pub fn load_project(root: &Path) -> Result<Project, Vec<Diagnostic>> {
    let mut project = Project::new();
    let mut diags = Vec::new();

    // Determine entry file and project root
    let entry_file: PathBuf;
    let project_root: PathBuf;

    if root.is_dir() {
        project_root = root.to_path_buf();
        let main_yml = root.join("main.yml");
        let main_yaml = root.join("main.yaml");
        if main_yml.exists() {
            entry_file = main_yml;
        } else if main_yaml.exists() {
            entry_file = main_yaml;
        } else {
            // No main file — backward compat: recursive walk of all .yml/.yaml files
            // without _use semantics (old behavior before _use was introduced)
            let mut files = Vec::new();
            walk_only(root, &mut files);
            files.sort();

            if files.is_empty() {
                return Ok(project);
            }

            project.root = project_root.clone();

            for path in &files {
                let file_id = FileId(project.files.len() as u32);
                project.files.push(path.clone());

                let contents = match fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(err) => {
                        diags.push(io_diagnostic(path, &err));
                        continue;
                    }
                };
                let node = match parse_document(&contents) {
                    Ok(n) => n,
                    Err(parse_err) => {
                        diags.push(parse_err.into_diagnostic(path.clone()));
                        continue;
                    }
                };

                let extract = extract_document(file_id, &node);
                let namespace = "";

                for def in extract.defs {
                    if let Err(dup) = project.namespaces.register(namespace, def) {
                        diags.push(dup.into_diagnostic(path.clone()));
                    }
                }
                for def in extract.file_scoped_defs {
                    if let Err(dup) = project.file_scoped.register(file_id, def) {
                        diags.push(dup.into_diagnostic(path.clone()));
                    }
                }
                for class in &extract.rejections {
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
                            if let Some(d) = class.clone().into_diagnostic(path.clone()) {
                                diags.push(d);
                            }
                        }
                    }
                }
                if let Some((fid, val)) = extract.meta_ymx.map(|mv| (file_id, mv.value)) {
                    project.raw_meta_ymx.push((fid, val));
                }
                if let Some((fid, val)) = extract.meta_test.map(|mv| (file_id, mv.value)) {
                    project.raw_meta_test.push((fid, val));
                }
            }

            return if diags.is_empty() {
                Ok(project)
            } else {
                Err(diags)
            };
        }
    } else {
        entry_file = root.to_path_buf();
        project_root = entry_file
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
    }

    project.root = project_root.clone();

    // Resolve the _use graph
    let file_paths = resolve_use_graph(&entry_file)?;

    // If no files (empty graph), return empty project
    if file_paths.is_empty() {
        return Ok(project);
    }

    // Map from file path -> (namespace_for_defs, extracted_defs, file_scoped_defs)
    // All defs go into the GLOBAL namespace regardless of where they were defined
    struct FileData {
        namespace: String,
        defs: Vec<ymx_core::namespace::Definition>,
        file_scoped_defs: Vec<ymx_core::namespace::Definition>,
        meta_ymx: Option<(FileId, Value)>,
        meta_test: Option<(FileId, Value)>,
    }

    let mut file_data_map: std::collections::HashMap<PathBuf, FileData> =
        std::collections::HashMap::new();

    // First pass: parse all files and extract their data
    for path in &file_paths {
        let file_id = FileId(project.files.len() as u32);
        project.files.push(path.clone());

        let contents = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(err) => {
                diags.push(io_diagnostic(path, &err));
                continue;
            }
        };
        let node = match parse_document(&contents) {
            Ok(n) => n,
            Err(parse_err) => {
                diags.push(parse_err.into_diagnostic(path.clone()));
                continue;
            }
        };

        let extract = extract_document(file_id, &node);

        // Classify rejections
        for class in &extract.rejections {
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
                    if let Some(d) = class.clone().into_diagnostic(path.clone()) {
                        diags.push(d);
                    }
                }
            }
        }

        let namespace = ""; // ALL defs go to global namespace

        file_data_map.insert(
            path.clone(),
            FileData {
                namespace: namespace.to_string(),
                defs: extract.defs,
                file_scoped_defs: extract.file_scoped_defs,
                meta_ymx: extract.meta_ymx.map(|mv| (file_id, mv.value)),
                meta_test: extract.meta_test.map(|mv| (file_id, mv.value)),
            },
        );
    }

    // Determine the entry file's _use directive
    // This determines which components are imported into the global namespace
    #[derive(Debug)]
    enum EntryImport {
        WildcardAll,                                               // _use: * or no _use — import all from all files
        WildcardFile,                                              // _use: {"*": "stem"} — import all from specific file
        NamedImports(Vec<(String, PathBuf, String)>), // (alias, target_path, component) — specific imports
    }

    let entry_raw_use = {
        let contents = match fs::read_to_string(&entry_file) {
            Ok(c) => c,
            Err(err) => {
                diags.push(io_diagnostic(&entry_file, &err));
                return Err(diags);
            }
        };
        let node = match parse_document(&contents) {
            Ok(n) => n,
            Err(parse_err) => {
                diags.push(parse_err.into_diagnostic(entry_file.clone()));
                return Err(diags);
            }
        };
        let extract = extract_document(FileId(0), &node);
        extract.meta_use.and_then(|mv| parse_raw_use(&mv.value))
    };

    let entry_import = match entry_raw_use {
        None | Some(RawUse::WildcardAll) => EntryImport::WildcardAll,
        Some(RawUse::WildcardFile(_)) => EntryImport::WildcardFile,
        Some(RawUse::NamedImports(imports)) => {
            // Validate named imports
            let mut validated = Vec::new();
            for (alias, file_path_str, component) in imports {
                let target_path = match resolve_file_stem(&file_path_str, &project_root) {
                    Ok(p) => p,
                    Err(e) => {
                        diags.push(e);
                        continue;
                    }
                };

                if let Some(target_data) = file_data_map.get(&target_path) {
                    let comp_exists = target_data.defs.iter().any(|d| d.full_name == component);
                    let comp_file_scoped = target_data
                        .file_scoped_defs
                        .iter()
                        .any(|d| d.full_name == component);

                    if comp_file_scoped {
                        diags.push(Diagnostic {
                            file: Some(entry_file.clone()),
                            line: 1,
                            col: 1,
                            component: Some(alias.clone()),
                            code: E005,
                            message: format!(
                                "cannot import file-scoped component `{}` from `{}` (prefix `_` is not importable)",
                                component,
                                target_path.display()
                            ),
                        });
                    } else if !comp_exists {
                        diags.push(Diagnostic {
                            file: Some(entry_file.clone()),
                            line: 1,
                            col: 1,
                            component: Some(alias.clone()),
                            code: E002,
                            message: format!(
                                "component `{}` not found in `{}`",
                                component,
                                target_path.display()
                            ),
                        });
                    } else {
                        validated.push((alias, target_path, component));
                    }
                } else {
                    diags.push(Diagnostic {
                        file: Some(entry_file.clone()),
                        line: 1,
                        col: 1,
                        component: Some(alias.clone()),
                        code: E002,
                        message: format!(
                            "component `{}` not found in `{}` (file not loaded)",
                            component,
                            target_path.display()
                        ),
                    });
                }
            }
            EntryImport::NamedImports(validated)
        }
    };

    // Register definitions based on entry's _use directive
    for path in &file_paths {
        let file_id = FileId(
            project
                .files
                .iter()
                .position(|p| p == path)
                .unwrap() as u32,
        );

        let data = match file_data_map.get(path) {
            Some(d) => d,
            None => continue,
        };

        // Determine if this file's defs should be registered
        // Entry file's defs are ALWAYS registered to global namespace
        let is_entry = path == &entry_file;
        let should_register = if is_entry {
            // Entry file's defs are always registered
            true
        } else {
            match &entry_import {
                EntryImport::WildcardAll | EntryImport::WildcardFile => {
                    // Wildcard imports: register all defs from all files in graph
                    true
                }
                EntryImport::NamedImports(_) => {
                    // Named imports: don't register non-entry files directly;
                    // only specific components via aliases
                    false
                }
            }
        };

        if should_register {
            for def in &data.defs {
                if let Err(dup) = project.namespaces.register(&data.namespace, def.clone()) {
                    diags.push(dup.into_diagnostic(path.clone()));
                }
            }
        }

        // File-scoped defs stay per-file
        for def in &data.file_scoped_defs {
            if let Err(dup) = project.file_scoped.register(file_id, def.clone()) {
                diags.push(dup.into_diagnostic(path.clone()));
            }
        }

        // Collect meta values
        if let Some((fid, val)) = data.meta_ymx.clone() {
            project.raw_meta_ymx.push((fid, val));
        }
        if let Some((fid, val)) = data.meta_test.clone() {
            project.raw_meta_test.push((fid, val));
        }
    }

    // Handle named imports: register specific components with alias names
    if let EntryImport::NamedImports(named_imports) = &entry_import {
        for (alias, target_path, component) in named_imports {
            if let Some(target_data) = file_data_map.get(target_path) {
                if let Some(def) = target_data.defs.iter().find(|d| d.full_name == *component) {
                    let mut aliased_def = def.clone();
                    aliased_def.full_name = alias.clone();
                    if let Err(dup) = project.namespaces.register("", aliased_def) {
                        diags.push(dup.into_diagnostic(target_path.clone()));
                    }
                }
            }
        }
    }

    if diags.is_empty() {
        Ok(project)
    } else {
        Err(diags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use ymx_core::diag::{E001, E002, E004, E005, E007, E009, E015};
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

    // ---- _use directive tests ----

    #[test]
    fn use_wildcard_all_loads_entry_plus_directory() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("a.yml", "a: 2\n");
        dir.write("subdir/b.yml", "b: 3\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        // All files in the same directory tree should be loaded
        assert!(project.namespaces.get("", "a").is_some(), "a should be global");
        assert!(project.namespaces.get("", "main").is_some(), "main should be global");
        assert!(project.namespaces.get("", "b").is_some(), "b should be global (from subdir)");
    }

    #[test]
    fn use_wildcard_file_imports_all_public_components() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": foo\nfoo: 1\na: 2\n");
        dir.write("foo.yml", "x: 10\ny: 20\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        assert!(project.namespaces.get("", "x").is_some(), "x imported");
        assert!(project.namespaces.get("", "y").is_some(), "y imported");
        assert!(project.namespaces.get("", "a").is_some(), "a still global");
    }

    #[test]
    fn use_named_import() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  sum: foo.bar\nfoo: 1\n");
        dir.write("foo.yml", "bar: 42\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        assert!(project.namespaces.get("", "sum").is_some(), "sum registered");
        let sum_def = project.namespaces.get("", "sum").unwrap();
        assert_eq!(sum_def.file, file_id_of(&project, "foo.yml"));
    }

    #[test]
    fn use_named_import_missing_component_is_e002() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  x: foo.bar\nfoo: 1\n");
        dir.write("foo.yml", "baz: 42\n"); // no `bar` component

        let err = load_project(&dir.path().join("main.yml"))
            .expect_err("missing component is E002");
        assert!(err.iter().any(|d| d.code == E002), "E002 for missing component");
    }

    #[test]
    fn use_named_import_file_scoped_is_e005() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  x: foo._bar\nfoo: 1\n");
        dir.write("foo.yml", "_bar: 42\n"); // file-scoped

        let err = load_project(&dir.path().join("main.yml"))
            .expect_err("file-scoped import is E005");
        assert!(err.iter().any(|d| d.code == E005), "E005 for file-scoped");
    }

    #[test]
    fn use_cycle_is_e001() {
        let dir = TempDir::new();
        dir.write("a.yml", "_use:\n  \"*\": b\nmain: 1\n");
        dir.write("b.yml", "_use:\n  \"*\": a\nx: 2\n");

        let err = load_project(&dir.path().join("a.yml"))
            .expect_err("cycle is E001");
        assert!(err.iter().any(|d| d.code == E001), "E001 for cycle");
    }

    #[test]
    fn use_transitive_imports() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": a\nmain: 1\n");
        dir.write("a.yml", "_use:\n  \"*\": b\n");
        dir.write("b.yml", "x: 42\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        assert!(project.namespaces.get("", "x").is_some(), "x from transitive b");
    }

    #[test]
    fn use_ambiguous_stem_is_e009() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": foo\nmain: 1\n");
        dir.write("foo.yml", "x: 1\n");
        dir.write("foo.yaml", "y: 2\n");

        let err = load_project(&dir.path().join("main.yml"))
            .expect_err("ambiguous stem is E009");
        assert!(err.iter().any(|d| d.code == E009), "E009 for ambiguity");
    }

    #[test]
    fn use_missing_file_is_e009() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": nonexistent\nmain: 1\n");

        let err = load_project(&dir.path().join("main.yml"))
            .expect_err("missing file is E009");
        assert!(err.iter().any(|d| d.code == E009), "E009 for missing file");
    }

    #[test]
    fn use_backward_compat_no_use_means_wildcard() {
        // If entry has no _use, behave as _use: *
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("other.yml", "other: 2\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        assert!(project.namespaces.get("", "main").is_some());
        assert!(project.namespaces.get("", "other").is_some());
    }

    #[test]
    fn use_directory_entry_falls_back_to_main() {
        // Passing a directory instead of a file should find main.yml or main.yaml
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("other.yml", "other: 2\n");

        let project = load_project(dir.path()).expect("loads via dir");
        assert!(project.namespaces.get("", "main").is_some());
        assert!(project.namespaces.get("", "other").is_some());
    }

    #[test]
    fn use_all_public_components_land_in_global_namespace() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": subdir.lib\nmain: 1\n");
        dir.write("subdir/lib.yml", "x: 10\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        // x should be in the global namespace, not subdir namespace
        assert!(
            project.namespaces.get("", "x").is_some(),
            "x is in global namespace"
        );
        assert!(
            project.namespaces.get("subdir", "x").is_none(),
            "x is NOT in subdir namespace"
        );
    }

    // ---- backward compat tests (existing behavior) ----

    #[test]
    fn loads_multi_file_tree_in_lex_order() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("a.yml", "a: 2\n");
        dir.write("subdir/b.yml", "b: 3\n");
        dir.write("subdir/nested/c.yml", "c: 4\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("loads cleanly");

        let expected: Vec<PathBuf> = ["a.yml", "main.yml", "subdir/b.yml", "subdir/nested/c.yml"]
            .iter()
            .map(|rel| dir.path().join(rel))
            .collect();
        assert_eq!(project.files, expected, "lexicographic path order");

        let a = project.namespaces.get("", "a").expect("a global");
        let main = project.namespaces.get("", "main").expect("main global");
        let b = project.namespaces.get("", "b").expect("b global");
        let c = project.namespaces.get("", "c").expect("c global");
        assert_eq!(a.file, FileId(0));
        assert_eq!(main.file, FileId(1));
        assert_eq!(b.file, FileId(2));
        assert_eq!(c.file, FileId(3));
    }

    #[test]
    fn parse_error_diagnostic_carries_resolved_path() {
        let dir = TempDir::new();
        dir.write("bad.yml", "a: 1\n---\nb: 2\n");

        let err = load_project(&dir.path().join("bad.yml")).expect_err("multi-doc stream is E001");
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
    fn duplicate_across_imported_files_is_e004() {
        // Test that importing a component with the same name as the entry file's own
        // component causes E004.
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  x: a.x\nx: 1\n");
        dir.write("a.yml", "x: 42\n");

        let err = load_project(&dir.path().join("main.yml"))
            .expect_err("duplicate in global namespace");
        assert_eq!(err.len(), 1);
        let diag = &err[0];
        assert_eq!(diag.code, E004);
        assert_eq!(diag.component.as_deref(), Some("x"));
    }

    #[test]
    fn file_scoped_defs_stay_out_of_namespaces() {
        let dir = TempDir::new();
        dir.write("main.yml", "_x: 1\nmain: 2\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");
        let main_id = file_id_of(&project, "main.yml");

        assert!(project.namespaces.get("", "_x").is_none());
        assert_eq!(
            project.file_scoped.get(main_id, "_x").unwrap().full_name,
            "_x"
        );
        assert_eq!(
            project.namespaces.get("", "main").unwrap().file,
            main_id
        );
    }

    #[test]
    fn e007_and_e015_rejections_carry_path_and_component() {
        let dir = TempDir::new();
        dir.write("e007.yml", "map: 1\nok: 2\n");

        let err = load_project(&dir.path().join("e007.yml")).expect_err("builtin name is E007");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E007);
        assert_eq!(err[0].component.as_deref(), Some("map"));

        let dir = TempDir::new();
        dir.write("e015.yml", "$_ymx: 1\n");

        let err = load_project(&dir.path().join("e015.yml")).expect_err("meta-key variant is E015");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E015);
        assert_eq!(err[0].component.as_deref(), Some("$_ymx"));
    }

    #[test]
    fn raw_meta_values_land_in_load_order() {
        let dir = TempDir::new();
        dir.write("a.yml", "_ymx:\n  v: 1\n_test:\n  t: a\na: 0\n");
        dir.write("m.yml", "_ymx:\n  v: 2\nm: 0\n");

        let project = load_project(&dir.path().join("a.yml")).expect("loads cleanly");
        let a_id = file_id_of(&project, "a.yml");
        let m_id = file_id_of(&project, "m.yml");

        assert_eq!(a_id, FileId(0));
        assert_eq!(m_id, FileId(1));

        assert_eq!(
            project.raw_meta_ymx,
            vec![(a_id, value_of("v: 1\n")), (m_id, value_of("v: 2\n")),],
            "_ymx values in load order"
        );
        assert_eq!(
            project.raw_meta_test,
            vec![(a_id, value_of("t: a\n")),],
            "_test values in load order"
        );
    }

    #[test]
    fn non_document_files_are_ignored() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("notes.txt", "not yaml\n");
        dir.write("README.md", "# readme\n");
        dir.write("subdir/data.yaml", "data: 2\n");
        dir.write("subdir/data.txt", "ignored\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

        assert!(project.namespaces.get("", "main").is_some());
        assert!(project.namespaces.get("", "data").is_some(), "data from subdir");
    }

    #[test]
    fn missing_entry_file_is_e001() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope.yml");

        let err = load_project(&missing).expect_err("missing entry cannot load");
        assert!(err.iter().any(|d| d.code == E001));
    }

    #[test]
    fn non_string_top_level_key_is_e001() {
        let dir = TempDir::new();
        dir.write("bad.yml", "0: a\nmain: 1\n");

        let err = load_project(&dir.path().join("bad.yml")).expect_err("non-string key is invalid");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E001);
    }

    #[test]
    fn meta_values_are_uninterpreted_verbatim() {
        let dir = TempDir::new();
        dir.write("main.yml", "_ymx: 5\n_test:\n  - 1\n  - 2\nmain: 0\n");

        let project = load_project(&dir.path().join("main.yml"))
            .expect("scalar and array meta load verbatim");
        assert_eq!(project.raw_meta_ymx[0].1, Value::Int(5));
        assert_eq!(
            project.raw_meta_test[0].1,
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }
}
