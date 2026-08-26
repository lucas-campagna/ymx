//! `ymx-web` — TypeScript/WASM library for YMX.
//!
//! Provides a `Ymx` class for browser/Node.js usage with:
//! - `parse(code)` to add YMX component definitions
//! - Dynamic component calls as methods: `ymx.component_name(args)`
//! - `${...}` math/context expressions evaluated in JavaScript

use std::sync::{Arc, Mutex};

use js_sys::JSON;
use wasm_bindgen::prelude::*;

use ymx_core::diag::{Diagnostic, FileId};
use ymx_core::exec::{CommandExecutor, ExecError, ExecOutput};
use ymx_core::ir::{Args, Value};
use ymx_core::namespace::Definition;
use ymx_core::parse::{Key, Node};
use ymx_core::project::{Options, PlainMode, Project};
use ymx_core::resolve::compile_component;

// Minimal allocator for WASM
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

/// JavaScript context executor for `${...}` expressions.
///
/// Evaluates expressions in JavaScript with access to:
/// - Browser globals (document, window, etc.)
/// - Positional args bound to `$0`, `$1`, etc.
/// - Named args bound to their names
#[derive(Debug)]
pub struct JsMathExecutor {
    /// Positional arguments for the current call
    args: Mutex<Vec<Value>>,
    /// Named arguments for the current call
    kwargs: Mutex<Vec<(String, Value)>>,
}

impl JsMathExecutor {
    pub fn new() -> Self {
        Self {
            args: Mutex::new(Vec::new()),
            kwargs: Mutex::new(Vec::new()),
        }
    }

    /// Set the arguments for the next call
    pub fn set_args(&self, args: Vec<Value>, kwargs: Vec<(String, Value)>) {
        *self.args.lock().unwrap() = args;
        *self.kwargs.lock().unwrap() = kwargs;
    }

    /// Convert a Value to a JS value
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

    /// Evaluate a JavaScript expression with bound arguments
    fn eval_js(&self, expr: &str) -> Result<String, ExecError> {
        let args = self.args.lock().unwrap();
        let kwargs = self.kwargs.lock().unwrap();

        // Bind positional args: $0, $1, etc.
        for (i, arg) in args.iter().enumerate() {
            let var_name = format!("${}", i);
            let js_val = Self::value_to_js(arg);
            let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str(&var_name), &js_val);
        }

        // Bind named args as variables
        for (name, value) in kwargs.iter() {
            let var_name = format!("${}", name);
            let js_val = Self::value_to_js(value);
            let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str(&var_name), &js_val);
        }

        // Evaluate the expression
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
    project: Project,
    options: Options,
    executor: Arc<JsMathExecutor>,
    last_result: Mutex<Option<Value>>,
}

#[wasm_bindgen]
impl Ymx {
    /// Create a new YMX instance
    #[wasm_bindgen(constructor)]
    pub fn new() -> Ymx {
        let executor = Arc::new(JsMathExecutor::new());
        let options = Options {
            entry: "main.main".to_string(),
            from_keyword: "from".to_string(),
            default_keyword: "default".to_string(),
            max_depth: 256,
            pretty: false,
            format: ymx_core::project::Format::Json,
            plain: PlainMode::False,
            allowed_backends: None,
            pdf_backend: "docker".to_string(),
            executor: Some(executor.clone()),
        };

        Ymx {
            project: Project::new(),
            options,
            executor,
            last_result: Mutex::new(None),
        }
    }

    /// Parse a YMX code string, adding or overwriting components.
    ///
    /// Subsequent calls with the same component name will overwrite previous definitions.
    ///
    /// Returns `Ok(null)` on success, or throws a JS Error with diagnostic info.
    pub fn parse(&mut self, code: &str) -> Result<(), JsValue> {
        let file_id = FileId(self.project.files.len() as u32);
        let path = std::path::PathBuf::from("<inline>");

        // Parse the YAML document
        let node = match ymx_core::parse::parse_document(code) {
            Ok(n) => n,
            Err(e) => {
                let diag = e.into_diagnostic(path);
                return Err(self.diagnostic_to_js(diag));
            }
        };

        // Register components into the project
        self.project.files.push(path.clone());

        // Walk the parsed document and register top-level definitions
        if let Node::Object(entries, _span) = node {
            for entry in entries {
                let name = Self::key_to_name(&entry.key);
                if name.is_empty() {
                    continue;
                }

                // Skip meta keys
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

                // Use register_override to allow overwriting
                self.project.namespaces.register_override("", def);
            }
        }

        Ok(())
    }

    /// Call a component by name with no arguments.
    /// Returns the result as a JSON string.
    pub fn call(&mut self, name: &str) -> Result<String, JsValue> {
        self.call_with_args(name, &JsValue::NULL)
    }

    /// Call a component by name with arguments.
    /// Arguments can be:
    /// - `null` or `undefined`: no arguments
    /// - An object: named arguments
    /// - An array: positional arguments
    ///
    /// Returns the result as a JSON string.
    pub fn call_with_args(&mut self, name: &str, args: &JsValue) -> Result<String, JsValue> {
        // Parse arguments
        let (args_vec, kwargs_vec) = Self::parse_js_args(args);

        // Set up executor with arguments
        self.executor.set_args(args_vec.clone(), kwargs_vec.clone());

        // Determine Args type
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

        // Compile and execute
        match compile_component(&self.project, name, &args, &self.options) {
            Ok(value) => {
                *self.last_result.lock().unwrap() = Some(value.clone());
                Ok(Self::value_to_json(&value))
            }
            Err(diags) => {
                if let Some(diag) = diags.first() {
                    Err(self.diagnostic_to_js(diag.clone()))
                } else {
                    Err(JsValue::from_str("Unknown error"))
                }
            }
        }
    }

    /// Get the last result value as a JSON string
    pub fn last_result(&self) -> Option<String> {
        self.last_result
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| Self::value_to_json(v))
    }

    // --- Private helper methods ---

    fn key_to_name(key: &Key) -> String {
        match key {
            Key::String(s) => s.clone(),
            Key::Int(i) => i.to_string(),
            _ => String::new(),
        }
    }

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

    fn value_to_json(v: &Value) -> String {
        serde_json::to_string(v).expect("failed to serialize value")
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
}

impl Default for Ymx {
    fn default() -> Self {
        Self::new()
    }
}
