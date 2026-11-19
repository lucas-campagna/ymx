//! `ymx-web` — TypeScript/WASM library for YMX.
//!
//! Provides a `Ymx` class for browser/Node.js usage with:
//! - `parse(code)` to add YMX component definitions
//! - Dynamic component calls: `ymx.component_name(args)`
//! - `${...}` math/context expressions evaluated in JavaScript
//! - Result objects with `.asHtml()` and `.asPdf()` methods
//! - Automatic ironpress initialization and cleanup

use std::sync::{Arc, Mutex, Once, RwLock};

use js_sys::JSON;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;

use ymx_core::diag::{Diagnostic, E001, E002, E004, E005, E007, E009, E010, E015, FileId};
use ymx_core::exec::{CommandExecutor, ExecError, ExecOutput};
use ymx_core::ir::{Args, Value};
use ymx_core::namespace::{Definition, extract_document, DefClass};
use ymx_core::parse::{Key, Node, parse_document};
use ymx_core::project::{Format, Options, PlainMode, Project};
use ymx_core::render::{DefaultHtmlRenderer, HtmlRenderer};
use ymx_core::resolve::compile_component;
use ymx_lint::{lint_project, LintConfig};

// Minimal allocator for WASM
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

// Panic hook initialization - runs once on first Ymx::new()
static INIT_PANIC_HOOK: Once = Once::new();

fn init_panic_hook() {
    INIT_PANIC_HOOK.call_once(|| {
        console_error_panic_hook::set_once();
    });
}

/// JavaScript context executor for `${...}` expressions.
#[derive(Debug)]
pub struct JsMathExecutor {
    args: Mutex<Vec<Value>>,
    kwargs: Mutex<Vec<(String, Value)>>,
}

impl JsMathExecutor {
    pub fn new() -> Self {
        Self {
            args: Mutex::new(Vec::new()),
            kwargs: Mutex::new(Vec::new()),
        }
    }

    pub fn set_args(&self, args: Vec<Value>, kwargs: Vec<(String, Value)>) {
        *self.args.lock().unwrap() = args;
        *self.kwargs.lock().unwrap() = kwargs;
    }

    fn value_to_js(v: &Value) -> JsValue {
        match v {
            Value::Null => JsValue::NULL,
            Value::Bool(b) => JsValue::from_bool(*b),
            Value::Int(i) => JsValue::from_f64(*i as f64),
            Value::Float(f) => JsValue::from_f64(*f),
            Value::String(s) => JsValue::from_str(s),
            Value::Array(arr) => {
                let js_arr = js_sys::Array::new();
                for item in arr {
                    js_arr.push(&Self::value_to_js(item));
                }
                js_arr.into()
            }
            Value::Object(map) => {
                let js_obj = js_sys::Object::new();
                for (k, v) in map {
                    let _ =
                        js_sys::Reflect::set(&js_obj, &JsValue::from_str(k), &Self::value_to_js(v));
                }
                js_obj.into()
            }
        }
    }

    fn eval_js(&self, expr: &str) -> Result<String, ExecError> {
        let args = self.args.lock().unwrap();
        let kwargs = self.kwargs.lock().unwrap();

        for (i, arg) in args.iter().enumerate() {
            let var_name = format!("${}", i);
            let js_val = Self::value_to_js(arg);
            let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str(&var_name), &js_val);
        }

        for (name, value) in kwargs.iter() {
            let var_name = format!("${}", name);
            let js_val = Self::value_to_js(value);
            let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str(&var_name), &js_val);
        }

        match js_sys::eval(expr) {
            Ok(result) => {
                let js_str = JSON::stringify(&result).expect("failed to stringify");
                Ok(js_str.as_string().unwrap_or_default())
            }
            Err(e) => {
                let msg = e
                    .as_string()
                    .unwrap_or_else(|| "unknown JS error".to_string());
                Err(ExecError::SpawnFailed(msg))
            }
        }
    }
}

impl Default for JsMathExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandExecutor for JsMathExecutor {
    fn execute(&self, _backend: &str, command: &str) -> Result<ExecOutput, ExecError> {
        let stdout = self.eval_js(command)?;
        Ok(ExecOutput {
            exit_code: 0,
            stdout,
            stderr: String::new(),
        })
    }
}

/// The main YMX WASM library class.
#[wasm_bindgen]
pub struct Ymx {
    project: Arc<RwLock<Project>>,
    options: Options,
    executor: Arc<JsMathExecutor>,
    last_result: Arc<Mutex<Option<Value>>>,
    ironpress_converter: Arc<Mutex<Option<js_sys::Object>>>,
}

#[wasm_bindgen]
impl Ymx {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Ymx {
        init_panic_hook();

        let executor = Arc::new(JsMathExecutor::new());
        let options = Options {
            entry: "main.main".to_string(),
            from_keyword: "from".to_string(),
            default_keyword: "default".to_string(),
            max_depth: 256,
            pretty: false,
            format: Format::Json,
            plain: PlainMode::False,
            allowed_backends: None,
            pdf_backend: "docker".to_string(),
            executor: Some(executor.clone()),
            allowed_ipc: None,
            ipc: None,
        };

        Ymx {
            project: Arc::new(RwLock::new(Project::new())),
            options,
            executor,
            last_result: Arc::new(Mutex::new(None)),
            ironpress_converter: Arc::new(Mutex::new(None)),
        }
    }

    pub fn parse(&mut self, code: &str) -> Result<(), JsValue> {
        let file_id = FileId(self.project.read().unwrap().files.len() as u32);
        let path = std::path::PathBuf::from("<inline>");

        let node = match ymx_core::parse::parse_document(code) {
            Ok(n) => n,
            Err(e) => {
                let diag = e.into_diagnostic(path);
                return Err(self.diagnostic_to_js(diag));
            }
        };

        self.project.write().unwrap().files.push(path.clone());

        if let Node::Object(entries, _span) = node {
            for entry in entries {
                let name = Self::key_to_name(&entry.key);
                if name.is_empty() {
                    continue;
                }
                if name == "_ymx" || name == "_test" || name == "_use" {
                    continue;
                }

                let def = Definition {
                    file: file_id,
                    full_name: name,
                    span: entry.key_span,
                    body: entry.value,
                    math_shorthand: false,
                    trailing_question: false,
                    exec_backend: None,
                };

                self.project
                    .write()
                    .unwrap()
                    .namespaces
                    .register_override("", def);
            }
        }

        Ok(())
    }

    /// Parse multiple files from a Map<path, content> for _use resolution.
    /// Entry point is main.yml (or main.yaml). All .yml files in the map are candidates.
    /// _use imports are resolved from main.yml.
    pub fn parse_map(&mut self, file_map: js_sys::Map) -> Result<(), JsValue> {
        // Convert JS Map to Rust HashMap (normalize paths by stripping leading /)
        let mut files: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let keys = js_sys::Object::keys(&file_map);
        for i in 0..keys.length() {
            let key = keys.get(i).as_string().unwrap_or_default();
            let normalized_key = key.strip_prefix('/').unwrap_or(&key);
            let js_key = JsValue::from_str(&key);
            let value = file_map.get(&js_key).as_string().unwrap_or_default();
            files.insert(normalized_key.to_string(), value);
        }

        // Find entry file (main.yml or main.yaml)
        let entry_path = if files.contains_key("main.yml") {
            "main.yml"
        } else if files.contains_key("main.yaml") {
            "main.yaml"
        } else {
            // No main file - treat all files as global (legacy behavior)
            return self.parse_map_all_files(files);
        };

        // Resolve _use graph from entry file
        let file_paths = match Ymx::resolve_use_graph(entry_path, &files) {
            Ok(paths) => paths,
            Err(diags) => return Err(self.diagnostics_to_js(diags)),
        };

        // Create new project (simpler than clearing)
        let mut project = Project::new();

        for (file_idx, path) in file_paths.iter().enumerate() {
            let file_id = FileId(file_idx as u32);
            project.files.push(std::path::PathBuf::from(path));

            let contents = match files.get(path) {
                Some(c) => c,
                None => continue,
            };

            let node = match parse_document(contents) {
                Ok(n) => n,
                Err(e) => return Err(self.diagnostic_to_js(e.into_diagnostic(std::path::PathBuf::from(path)))),
            };

            let extract = extract_document(file_id, &node);

            // Check for invalid names
            for class in &extract.rejections {
                match class {
                    DefClass::InvalidName(span) => {
                        let diag = Diagnostic {
                            file: Some(std::path::PathBuf::from(path)),
                            line: span.line,
                            col: span.col,
                            component: None,
                            code: E001,
                            message: "invalid top-level name (must match `[$]*[A-Za-z_][A-Za-z0-9_]*`; a non-string key cannot name a component or template)".to_string(),
                        };
                        return Err(self.diagnostic_to_js(diag));
                    }
                    class => {
                        if let Some(d) = class.clone().into_diagnostic(std::path::PathBuf::from(path)) {
                            return Err(self.diagnostic_to_js(d));
                        }
                    }
                }
            }

            // Register all definitions in global namespace (register_override never fails)
            for def in extract.defs {
                project.namespaces.register_override("", def);
            }
        }

        // Replace the project
        *self.project.write().unwrap() = project;

        Ok(())
    }

    fn parse_map_all_files(&mut self, files: std::collections::HashMap<String, String>) -> Result<(), JsValue> {
        let mut project = Project::new();

        for (file_idx, (path, contents)) in files.iter().enumerate() {
            let file_id = FileId(file_idx as u32);
            project.files.push(std::path::PathBuf::from(path));

            let node = match parse_document(contents) {
                Ok(n) => n,
                Err(e) => return Err(self.diagnostic_to_js(e.into_diagnostic(std::path::PathBuf::from(path)))),
            };

            if let Node::Object(entries, _span) = node {
                for entry in entries {
                    let name = Self::key_to_name(&entry.key);
                    if name.is_empty() || name == "_ymx" || name == "_test" || name == "_use" {
                        continue;
                    }

                    let def = Definition {
                        file: file_id,
                        full_name: name,
                        span: entry.key_span,
                        body: entry.value,
                        math_shorthand: false,
                        trailing_question: false,
                        exec_backend: None,
                    };

                    project.namespaces.register_override("", def);
                }
            }
        }

        *self.project.write().unwrap() = project;
        Ok(())
    }

    /// Resolve the _use graph starting from entry file.
    /// Returns sorted list of file paths that should be loaded.
    fn resolve_use_graph(
        entry_path: &str,
        files: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<String>, Vec<Diagnostic>> {
        let mut diags = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut result = Vec::new();

        // Helper to resolve file stem to actual path
        let resolve_stem = |stem: &str| -> Option<String> {
            // Try exact match first
            if files.contains_key(stem) {
                return Some(stem.to_string());
            }
            // Try with .yml
            let with_yml = format!("{}.yml", stem);
            if files.contains_key(&with_yml) {
                return Some(with_yml);
            }
            // Try with .yaml
            let with_yaml = format!("{}.yaml", stem);
            if files.contains_key(&with_yaml) {
                return Some(with_yaml);
            }
            None
        };

        // Recursive function to follow _use imports
        fn visit(
            path: &str,
            files: &std::collections::HashMap<String, String>,
            visited: &mut std::collections::HashSet<String>,
            result: &mut Vec<String>,
            diags: &mut Vec<Diagnostic>,
            resolve_stem: &dyn Fn(&str) -> Option<String>,
        ) {
            if visited.contains(path) {
                return;
            }
            visited.insert(path.to_string());

            // Parse to extract _use
            let contents = match files.get(path) {
                Some(c) => c,
                None => return,
            };

            let node = match parse_document(contents) {
                Ok(n) => n,
                Err(e) => {
                    diags.push(e.into_diagnostic(std::path::PathBuf::from(path)));
                    return;
                }
            };

            // Extract _use from the document
            let extract = extract_document(FileId(0), &node);
            if let Some(meta_use) = extract.meta_use {
                let raw_use = parse_raw_use(&meta_use.value);
                if let Some(raw_use) = raw_use {
                    match raw_use {
                        RawUse::WildcardAll => {
                            // Import all files
                            for file_path in files.keys() {
                                visit(file_path, files, visited, result, diags, resolve_stem);
                            }
                        }
                        RawUse::WildcardFile(stem) => {
                            if let Some(target) = resolve_stem(&stem) {
                                visit(&target, files, visited, result, diags, resolve_stem);
                            }
                        }
                        RawUse::NamedImports(imports) => {
                            for (_alias, file_stem, _component) in imports {
                                if let Some(target) = resolve_stem(&file_stem) {
                                    visit(&target, files, visited, result, diags, resolve_stem);
                                }
                            }
                        }
                        RawUse::IpcDeclarations(_) => {
                            // IPC declarations don't import files
                        }
                    }
                }
            }

            // Add this file to result after visiting its imports (so imports come first)
            result.push(path.to_string());
        }

        visit(entry_path, files, &mut visited, &mut result, &mut diags, &resolve_stem);

        if !diags.is_empty() {
            return Err(diags);
        }

        Ok(result)
    }

    /// Clear all parsed components and reset the project.
    pub fn clear(&mut self) {
        self.project = Arc::new(RwLock::new(Project::new()));
    }

    /// Get a callable proxy for a component.
    /// Usage: `ymx.greet({ name: "World" })` or `ymx.add(1, 2)`
    /// Returns a result object with `.asHtml()` and `.asPdf()` methods.
    pub fn __get__(&self, name: &str) -> Result<js_sys::Function, JsValue> {
        let name = name.to_string();
        let project = self.project.clone();
        let options = self.options.clone();
        let executor = self.executor.clone();
        let last_result = self.last_result.clone();
        let ironpress_converter = self.ironpress_converter.clone();

        let closure = Closure::wrap(Box::new(move |_this: JsValue, args: js_sys::Array| {
            let (args_vec, kwargs_vec) = Self::parse_js_args_from_array(&args);

            executor.set_args(args_vec.clone(), kwargs_vec.clone());

            let args = if kwargs_vec.is_empty() && !args_vec.is_empty() {
                Args::Positional(args_vec)
            } else if args_vec.is_empty() && !kwargs_vec.is_empty() {
                Args::Named(kwargs_vec)
            } else if !args_vec.is_empty() && !kwargs_vec.is_empty() {
                Args::Mixed {
                    named: kwargs_vec,
                    positional: args_vec,
                }
            } else {
                Args::None
            };

            match compile_component(&*project.read().unwrap(), &name, &args, &options) {
                Ok(value) => {
                    *last_result.lock().unwrap() = Some(value.clone());

                    let result_obj = js_sys::Object::new();
                    let value_for_html = value.clone();
                    let value_for_pdf = value.clone();
                    let value_for_json = value.clone();
                    let ironpress_converter_clone = ironpress_converter.clone();

                    // .asHtml() method
                    let as_html = Closure::wrap(Box::new(move || {
                        DefaultHtmlRenderer.render_html(&value_for_html)
                    }) as Box<dyn Fn() -> String>);
                    js_sys::Reflect::set(
                        &result_obj,
                        &"asHtml".into(),
                        as_html.as_ref().unchecked_ref(),
                    )
                    .unwrap();
                    as_html.forget();

                    // .asPdf() method
                    let as_pdf = Closure::wrap(Box::new(move || {
                        let html = DefaultHtmlRenderer.render_html(&value_for_pdf);
                        let mut converter_guard = ironpress_converter_clone.lock().unwrap();
                        let converter = if let Some(c) = converter_guard.as_ref() {
                            c.clone()
                        } else {
                            let global = js_sys::global();
                            let ironpress =
                                js_sys::Reflect::get(&global, &"ironpress".into()).unwrap();
                            let html_converter =
                                js_sys::Reflect::get(&ironpress, &"HtmlConverter".into()).unwrap();
                            let converter: js_sys::Object = js_sys::Reflect::construct(
                                &html_converter.dyn_into::<js_sys::Function>().unwrap(),
                                &js_sys::Array::new(),
                            )
                            .unwrap()
                            .into();
                            *converter_guard = Some(converter.clone());
                            converter
                        };
                        drop(converter_guard);

                        let html_to_pdf =
                            js_sys::Reflect::get(&converter, &"htmlToPdf".into()).unwrap();
                        let html_to_pdf_fn = js_sys::Function::from(html_to_pdf);
                        let pdf_result = html_to_pdf_fn
                            .call1(&converter, &JsValue::from_str(&html))
                            .unwrap();

                        js_sys::Uint8Array::from(pdf_result)
                    })
                        as Box<dyn Fn() -> js_sys::Uint8Array>);
                    js_sys::Reflect::set(
                        &result_obj,
                        &"asPdf".into(),
                        as_pdf.as_ref().unchecked_ref(),
                    )
                    .unwrap();
                    as_pdf.forget();

                    // Also include the raw JSON value
                    let json_str = serde_json::to_string(&value_for_json).unwrap();
                    js_sys::Reflect::set(&result_obj, &"json".into(), &json_str.into()).unwrap();

                    result_obj.into()
                }
                Err(diags) => {
                    if let Some(diag) = diags.first() {
                        JsValue::from_str(&format!("YMX Error [{}]: {}", diag.code, diag.message))
                    } else {
                        JsValue::from_str("Unknown error")
                    }
                }
            }
        })
            as Box<dyn FnMut(JsValue, js_sys::Array) -> JsValue>);

        Ok(closure.into_js_value().unchecked_into::<js_sys::Function>())
    }

    pub fn components(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        if let Some(ns) = self.project.read().unwrap().namespaces.namespace("") {
            for (name, _) in ns.defs() {
                arr.push(&JsValue::from_str(name));
            }
        }
        arr
    }

    /// Lint the current project and return findings as a JavaScript object.
    /// Returns an object with `findings` array and `diagnostics` array.
    pub fn lint(&self) -> JsValue {
        let project = self.project.read().unwrap();
        let options = self.options.clone();
        let config = LintConfig::default();
        
        let report = lint_project(&project, &options, &config);
        
        let result = js_sys::Object::new();
        
        // Convert findings
        let findings_arr = js_sys::Array::new();
        for finding in report.findings {
            let finding_obj = js_sys::Object::new();
            js_sys::Reflect::set(&finding_obj, &"code".into(), &finding.code.into()).unwrap();
            js_sys::Reflect::set(&finding_obj, &"severity".into(), &format!("{:?}", finding.severity).into()).unwrap();
            if let Some(file) = finding.file {
                js_sys::Reflect::set(&finding_obj, &"file".into(), &file.to_string_lossy().to_string().into()).unwrap();
            }
            js_sys::Reflect::set(&finding_obj, &"line".into(), &finding.line.into()).unwrap();
            js_sys::Reflect::set(&finding_obj, &"col".into(), &finding.col.into()).unwrap();
            if let Some(component) = finding.component {
                js_sys::Reflect::set(&finding_obj, &"component".into(), &component.into()).unwrap();
            }
            js_sys::Reflect::set(&finding_obj, &"message".into(), &finding.message.into()).unwrap();
            if let Some(help) = finding.help {
                js_sys::Reflect::set(&finding_obj, &"help".into(), &help.into()).unwrap();
            }
            findings_arr.push(&finding_obj);
        }
        js_sys::Reflect::set(&result, &"findings".into(), &findings_arr).unwrap();
        
        // Convert diagnostics
        let diagnostics_arr = js_sys::Array::new();
        for diag in report.diagnostics {
            let diag_obj = js_sys::Object::new();
            js_sys::Reflect::set(&diag_obj, &"code".into(), &diag.code.into()).unwrap();
            if let Some(file) = diag.file {
                js_sys::Reflect::set(&diag_obj, &"file".into(), &file.to_string_lossy().to_string().into()).unwrap();
            }
            js_sys::Reflect::set(&diag_obj, &"line".into(), &diag.line.into()).unwrap();
            js_sys::Reflect::set(&diag_obj, &"col".into(), &diag.col.into()).unwrap();
            if let Some(component) = diag.component {
                js_sys::Reflect::set(&diag_obj, &"component".into(), &component.into()).unwrap();
            }
            js_sys::Reflect::set(&diag_obj, &"message".into(), &diag.message.into()).unwrap();
            diagnostics_arr.push(&diag_obj);
        }
        js_sys::Reflect::set(&result, &"diagnostics".into(), &diagnostics_arr).unwrap();
        
        result.into()
    }

    fn key_to_name(key: &Key) -> String {
        match key {
            Key::String(s) => s.clone(),
            Key::Int(i) => i.to_string(),
            _ => String::new(),
        }
    }

    fn parse_js_args_from_array(args: &js_sys::Array) -> (Vec<Value>, Vec<(String, Value)>) {
        let mut values = Vec::new();
        let mut kwargs = Vec::new();
        for i in 0..args.length() {
            let val = args.get(i);
            if let Some(v) = Self::js_to_value(val) {
                values.push(v);
            }
        }
        if let Some(last) = values.last() {
            if matches!(last, Value::Object(_)) {
                if let Value::Object(map) = values.pop().unwrap() {
                    for (k, v) in map {
                        kwargs.push((k, v));
                    }
                }
            }
        }
        (values, kwargs)
    }

    #[allow(dead_code)]
    fn parse_js_args(args: &JsValue) -> (Vec<Value>, Vec<(String, Value)>) {
        if args.is_null() || args.is_undefined() {
            return (Vec::new(), Vec::new());
        }
        if args.is_object() {
            if js_sys::Array::is_array(args) {
                let arr = js_sys::Array::from(args);
                let mut values = Vec::new();
                for i in 0..arr.length() {
                    if let Some(v) = Self::js_to_value(arr.get(i)) {
                        values.push(v);
                    }
                }
                return (values, Vec::new());
            }
            let obj = js_sys::Object::from(args.clone());
            let mut kwargs = Vec::new();
            let keys = js_sys::Object::keys(&obj);
            for i in 0..keys.length() {
                let key = keys.get(i).as_string().unwrap_or_default();
                let value = js_sys::Reflect::get(&obj, &keys.get(i)).unwrap_or(JsValue::NULL);
                if let Some(v) = Self::js_to_value(value) {
                    kwargs.push((key, v));
                }
            }
            return (Vec::new(), kwargs);
        }
        (Vec::new(), Vec::new())
    }

    fn js_to_value(v: JsValue) -> Option<Value> {
        if v.is_null() || v.is_undefined() {
            Some(Value::Null)
        } else if let Some(b) = v.as_bool() {
            Some(Value::Bool(b))
        } else if let Some(n) = v.as_f64() {
            if n.fract() == 0.0 && n.abs() < (i64::MAX as f64) {
                Some(Value::Int(n as i64))
            } else {
                Some(Value::Float(n))
            }
        } else if let Some(s) = v.as_string() {
            Some(Value::String(s))
        } else if js_sys::Array::is_array(&v) {
            let arr = js_sys::Array::from(&v);
            let mut values = Vec::new();
            for i in 0..arr.length() {
                if let Some(val) = Self::js_to_value(arr.get(i)) {
                    values.push(val);
                }
            }
            Some(Value::Array(values))
        } else if v.is_object() {
            let obj = js_sys::Object::from(v);
            let mut map = indexmap::IndexMap::new();
            let keys = js_sys::Object::keys(&obj);
            for i in 0..keys.length() {
                let key = keys.get(i).as_string().unwrap_or_default();
                let value = js_sys::Reflect::get(&obj, &keys.get(i)).unwrap_or(JsValue::NULL);
                if let Some(val) = Self::js_to_value(value) {
                    map.insert(key, val);
                }
            }
            Some(Value::Object(map))
        } else {
            None
        }
    }

    fn diagnostic_to_js(&self, diag: Diagnostic) -> JsValue {
        let msg = format!(
            "[{}] {}:{}:{} ({}): {}",
            diag.code,
            diag.file
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            diag.line,
            diag.col,
            diag.component.unwrap_or_default(),
            diag.message
        );
        JsValue::from_str(&msg)
    }

    fn diagnostics_to_js(&self, diags: Vec<Diagnostic>) -> JsValue {
        let arr = js_sys::Array::new();
        for diag in diags {
            arr.push(&self.diagnostic_to_js(diag));
        }
        arr.into()
    }
}

/// Raw _use directive variants.
#[derive(Debug)]
enum RawUse {
    WildcardAll,
    WildcardFile(String),
    NamedImports(Vec<(String, String, String)>), // (alias, file_stem, component)
    IpcDeclarations(Vec<(String, Value)>),
}

/// Parse a raw _use value into a RawUse enum.
fn parse_raw_use(value: &Value) -> Option<RawUse> {
    match value {
        Value::String(s) if s == "*" => Some(RawUse::WildcardAll),
        Value::String(s) => Some(RawUse::WildcardFile(s.clone())),
        Value::Object(map) => {
            // Check if it's IPC declarations or named imports
            let has_ipc = map.values().any(|v| {
                matches!(v, Value::Object(obj) if obj.contains_key("cmd") || obj.contains_key("transport"))
            });
            if has_ipc {
                let mut decls = Vec::new();
                for (alias, spec) in map {
                    decls.push((alias.clone(), spec.clone()));
                }
                Some(RawUse::IpcDeclarations(decls))
            } else {
                let mut imports = Vec::new();
                for (alias, target) in map {
                    if let Value::String(target_str) = target {
                        // target can be "file.component" or just "file"
                        let parts: Vec<&str> = target_str.split('.').collect();
                        if parts.len() >= 2 {
                            imports.push((alias.clone(), parts[0].to_string(), parts[1..].join(".")));
                        } else {
                            imports.push((alias.clone(), parts[0].to_string(), String::new()));
                        }
                    }
                }
                Some(RawUse::NamedImports(imports))
            }
        }
        _ => None,
    }
}

impl Drop for Ymx {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.ironpress_converter.lock() {
            if let Some(converter) = guard.take() {
                let free_fn = js_sys::Reflect::get(&converter, &"free".into()).unwrap();
                if !free_fn.is_undefined() && !free_fn.is_null() {
                    let free_fn = js_sys::Function::from(free_fn);
                    let _ = free_fn.call0(&converter);
                }
            }
        }
    }
}

impl Default for Ymx {
    fn default() -> Self {
        Self::new()
    }
}
