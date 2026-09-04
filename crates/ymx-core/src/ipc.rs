//! IPC (Inter-Process Communication) types for rule-21 external components.
//!
//! This module is **I/O-free** — it defines the specification, request/response
//! types, error variants, and the [`IpcHost`] trait. Concrete implementations
//! live in `ymx-lib` (`StdIpcHost`) or other I/O-capable crates.

use std::fmt;

use indexmap::IndexMap;

use crate::diag::{Diagnostic, E010};
use crate::ir::Value;

// ---------------------------------------------------------------------------
// Enums (Task 1.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum IpcRunner {
    Process,
    External,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcTransport {
    Pipe,
    Socket,
    #[cfg(feature = "http")]
    Http,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcProtocol {
    Line,
    Sentinel,
    Raw,
    Json,
    Jsonrpc,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcMode {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcParse {
    None,
    Yaml,
    Json,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcEnvelope {
    Payload,
    Full,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcStderr {
    Ignore,
    Capture,
    Fail,
}

#[cfg(feature = "http")]
#[derive(Debug, Clone, PartialEq)]
pub enum IpcHttpBody {
    All,
    Positional,
    Named(String),
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcRestart {
    Never,
    OnFailure,
}

// ---------------------------------------------------------------------------
// IpcSpec (Task 1.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IpcSpec {
    pub runner: IpcRunner,
    pub transport: IpcTransport,
    pub protocol: IpcProtocol,
    pub request_template: Option<String>,
    pub reply_until: Option<String>,
    pub mode: IpcMode,
    pub on_request: Option<String>,
    pub parse: IpcParse,
    pub trim: bool,
    pub error_pattern: Option<String>,
    pub envelope: IpcEnvelope,
    pub stderr: IpcStderr,
    pub on_response: Option<String>,
    pub on_error: Option<String>,
    pub startup_timeout: Option<u64>,
    pub ready: Option<String>,
    pub request_timeout: Option<u64>,
    pub stop_signal: Option<String>,
    pub stop_message: Option<String>,
    pub stop_timeout: Option<u64>,
    pub before_start: Option<String>,
    pub after_start: Option<String>,
    pub before_stop: Option<String>,
    pub after_stop: Option<String>,
    pub prelude: Option<String>,
    #[cfg(feature = "http")]
    pub url: Option<String>,
    #[cfg(feature = "http")]
    pub method: Option<String>,
    #[cfg(feature = "http")]
    pub headers: Option<IndexMap<String, Value>>,
    #[cfg(feature = "http")]
    pub query: Option<Vec<String>>,
    #[cfg(feature = "http")]
    pub body: Option<IpcHttpBody>,
    #[cfg(feature = "http")]
    pub ok_status: Option<String>,
    pub addr: Option<String>,
    pub path: Option<String>,
    pub env: Option<IndexMap<String, Value>>,
    pub cwd: Option<String>,
    pub cmd: Option<Value>,
    pub shell: bool,
    pub coproc: bool,
    pub restart: IpcRestart,
    pub max_restarts: u32,
    pub lazy: bool,
    pub external: bool,
}

impl Default for IpcSpec {
    fn default() -> Self {
        IpcSpec {
            runner: IpcRunner::Process,
            transport: IpcTransport::Pipe,
            protocol: IpcProtocol::Line,
            request_template: None,
            reply_until: None,
            mode: IpcMode::Text,
            on_request: None,
            parse: IpcParse::Yaml,
            trim: true,
            error_pattern: None,
            envelope: IpcEnvelope::Payload,
            stderr: IpcStderr::Ignore,
            on_response: None,
            on_error: None,
            startup_timeout: None,
            ready: None,
            request_timeout: None,
            stop_signal: None,
            stop_message: None,
            stop_timeout: None,
            before_start: None,
            after_start: None,
            before_stop: None,
            after_stop: None,
            prelude: None,
            #[cfg(feature = "http")]
            url: None,
            #[cfg(feature = "http")]
            method: None,
            #[cfg(feature = "http")]
            headers: None,
            #[cfg(feature = "http")]
            query: None,
            #[cfg(feature = "http")]
            body: None,
            #[cfg(feature = "http")]
            ok_status: None,
            addr: None,
            path: None,
            env: None,
            cwd: None,
            cmd: None,
            shell: false,
            coproc: false,
            restart: IpcRestart::Never,
            max_restarts: 0,
            lazy: true,
            external: false,
        }
    }
}

// ---------------------------------------------------------------------------
// IpcRequest / IpcResponse / IpcError (Tasks 1.2, 1.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IpcRequest {
    pub args: IndexMap<String, Value>,
    /// Optional pre-rendered request body. When Some, this is sent directly
    /// as the wire format, bypassing args serialization. Used by on_request
    /// override and by json/jsonrpc protocols.
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IpcResponse {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<u16>,
}

#[derive(Debug)]
pub enum IpcError {
    NoHost,
    DisallowedTransport(String),
    SpawnFailed(String),
    Crashed,
    Timeout(u64),
    FramingError(String),
    StatusCode(u16, String),
    HookFailed(String),
    Custom(String),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::NoHost => write!(f, "IPC disabled (no IpcHost provided)"),
            IpcError::DisallowedTransport(name) => {
                write!(f, "transport '{name}' is not allowed")
            }
            IpcError::SpawnFailed(reason) => write!(f, "IPC spawn failed: {reason}"),
            IpcError::Crashed => write!(f, "IPC process crashed"),
            IpcError::Timeout(ms) => write!(f, "IPC timeout after {ms}ms"),
            IpcError::FramingError(reason) => write!(f, "IPC protocol error: {reason}"),
            IpcError::StatusCode(code, body) => {
                write!(f, "IPC HTTP status {code}: {body}")
            }
            IpcError::HookFailed(reason) => write!(f, "IPC hook failed: {reason}"),
            IpcError::Custom(msg) => write!(f, "{msg}"),
        }
    }
}

// ---------------------------------------------------------------------------
// IpcHost trait (Task 1.4)
// ---------------------------------------------------------------------------

pub trait IpcHost: Send + Sync + fmt::Debug {
    fn call(
        &self,
        name: &str,
        spec: &IpcSpec,
        request: IpcRequest,
    ) -> Result<IpcResponse, IpcError>;

    fn shutdown(&self);
}

// ---------------------------------------------------------------------------
// parse_ipc_spec (Task 1.6)
// ---------------------------------------------------------------------------

fn string_value(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        _ => None,
    }
}

fn parse_runner(v: &Value) -> Result<IpcRunner, String> {
    match string_value(v) {
        Some("process") => Ok(IpcRunner::Process),
        Some("external") => Ok(IpcRunner::External),
        Some(other) => Err(format!("unknown runner '{other}'")),
        None => Err("runner must be a string".to_string()),
    }
}

fn parse_transport(v: &Value) -> Result<IpcTransport, String> {
    match string_value(v) {
        Some("pipe") => Ok(IpcTransport::Pipe),
        Some("socket") => Ok(IpcTransport::Socket),
        #[cfg(feature = "http")]
        Some("http") => Ok(IpcTransport::Http),
        Some(other) => Err(format!("unknown transport '{other}'")),
        None => Err("transport must be a string".to_string()),
    }
}

fn parse_protocol(v: &Value) -> Result<IpcProtocol, String> {
    match string_value(v) {
        Some("line") => Ok(IpcProtocol::Line),
        Some("sentinel") => Ok(IpcProtocol::Sentinel),
        Some("raw") => Ok(IpcProtocol::Raw),
        Some("json") => Ok(IpcProtocol::Json),
        Some("jsonrpc") => Ok(IpcProtocol::Jsonrpc),
        Some(other) => Err(format!("unknown protocol '{other}'")),
        None => Err("protocol must be a string".to_string()),
    }
}

fn parse_mode(v: &Value) -> Result<IpcMode, String> {
    match string_value(v) {
        Some("text") => Ok(IpcMode::Text),
        Some("json") => Ok(IpcMode::Json),
        Some(other) => Err(format!("unknown mode '{other}'")),
        None => Err("mode must be a string".to_string()),
    }
}

fn parse_parse(v: &Value) -> Result<IpcParse, String> {
    match string_value(v) {
        Some("none") => Ok(IpcParse::None),
        Some("yaml") => Ok(IpcParse::Yaml),
        Some("json") => Ok(IpcParse::Json),
        Some(other) => Err(format!("unknown parse '{other}'")),
        None => Err("parse must be a string".to_string()),
    }
}

fn parse_envelope(v: &Value) -> Result<IpcEnvelope, String> {
    match string_value(v) {
        Some("payload") => Ok(IpcEnvelope::Payload),
        Some("full") => Ok(IpcEnvelope::Full),
        Some(other) => Err(format!("unknown envelope '{other}'")),
        None => Err("envelope must be a string".to_string()),
    }
}

fn parse_stderr(v: &Value) -> Result<IpcStderr, String> {
    match string_value(v) {
        Some("ignore") => Ok(IpcStderr::Ignore),
        Some("capture") => Ok(IpcStderr::Capture),
        Some("fail") => Ok(IpcStderr::Fail),
        Some(other) => Err(format!("unknown stderr '{other}'")),
        None => Err("stderr must be a string".to_string()),
    }
}

fn parse_restart(v: &Value) -> Result<IpcRestart, String> {
    match string_value(v) {
        Some("never") => Ok(IpcRestart::Never),
        Some("on-failure") => Ok(IpcRestart::OnFailure),
        Some(other) => Err(format!("unknown restart '{other}'")),
        None => Err("restart must be a string".to_string()),
    }
}

#[cfg(feature = "http")]
fn parse_http_body(v: &Value) -> Result<IpcHttpBody, String> {
    match v {
        Value::String(s) => match s.as_str() {
            "all" => Ok(IpcHttpBody::All),
            "positional" => Ok(IpcHttpBody::Positional),
            "off" => Ok(IpcHttpBody::Off),
            other => Ok(IpcHttpBody::Named(other.to_string())),
        },
        _ => Err("body must be a string".to_string()),
    }
}

fn optional_string(
    map: &IndexMap<String, Value>,
    key: &str,
) -> Result<Option<String>, Vec<Diagnostic>> {
    match map.get(key) {
        Some(v) => match string_value(v) {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("'{key}' must be a string"),
            }]),
        },
        None => Ok(None),
    }
}

fn optional_bool(map: &IndexMap<String, Value>, key: &str) -> Result<bool, Vec<Diagnostic>> {
    match map.get(key) {
        Some(v) => match v {
            Value::Bool(b) => Ok(*b),
            Value::String(s) => match s.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(vec![Diagnostic {
                    file: None,
                    line: 0,
                    col: 0,
                    component: None,
                    code: E010,
                    message: format!("'{key}' must be a boolean or 'true'/'false' string"),
                }]),
            },
            _ => Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("'{key}' must be a boolean"),
            }]),
        },
        None => Ok(false),
    }
}

fn optional_u64(map: &IndexMap<String, Value>, key: &str) -> Result<Option<u64>, Vec<Diagnostic>> {
    match map.get(key) {
        Some(v) => match v {
            Value::Int(n) => {
                if *n < 0 {
                    Err(vec![Diagnostic {
                        file: None,
                        line: 0,
                        col: 0,
                        component: None,
                        code: E010,
                        message: format!("'{key}' must be a non-negative integer"),
                    }])
                } else {
                    Ok(Some(*n as u64))
                }
            }
            Value::Float(f) => {
                let truncated = *f as u64;
                if truncated as f64 != *f || *f < 0.0 {
                    Err(vec![Diagnostic {
                        file: None,
                        line: 0,
                        col: 0,
                        component: None,
                        code: E010,
                        message: format!("'{key}' must be a non-negative integer"),
                    }])
                } else {
                    Ok(Some(truncated))
                }
            }
            _ => Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("'{key}' must be a non-negative integer"),
            }]),
        },
        None => Ok(None),
    }
}

fn optional_u32(
    map: &IndexMap<String, Value>,
    key: &str,
    default: u32,
) -> Result<u32, Vec<Diagnostic>> {
    match map.get(key) {
        Some(v) => match v {
            Value::Int(n) => {
                if *n < 0 {
                    Err(vec![Diagnostic {
                        file: None,
                        line: 0,
                        col: 0,
                        component: None,
                        code: E010,
                        message: format!("'{key}' must be a non-negative integer"),
                    }])
                } else {
                    Ok(*n as u32)
                }
            }
            _ => Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("'{key}' must be a non-negative integer"),
            }]),
        },
        None => Ok(default),
    }
}

fn optional_map(
    map: &IndexMap<String, Value>,
    key: &str,
) -> Result<Option<IndexMap<String, Value>>, Vec<Diagnostic>> {
    match map.get(key) {
        Some(v) => match v {
            Value::Object(m) => Ok(Some(m.clone())),
            _ => Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("'{key}' must be a mapping"),
            }]),
        },
        None => Ok(None),
    }
}

#[cfg(feature = "http")]
fn optional_string_vec(
    map: &IndexMap<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, Vec<Diagnostic>> {
    match map.get(key) {
        Some(v) => match v {
            Value::Array(arr) => {
                let mut result = Vec::new();
                for item in arr {
                    match string_value(item) {
                        Some(s) => result.push(s.to_string()),
                        None => {
                            return Err(vec![Diagnostic {
                                file: None,
                                line: 0,
                                col: 0,
                                component: None,
                                code: E010,
                                message: format!("each element of '{key}' must be a string"),
                            }])
                        }
                    }
                }
                Ok(Some(result))
            }
            _ => Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("'{key}' must be an array of strings"),
            }]),
        },
        None => Ok(None),
    }
}

/// Known IPC spec field names for unknown-field detection.
const KNOWN_FIELDS: &[&str] = &[
    "runner",
    "transport",
    "protocol",
    "request_template",
    "reply_until",
    "mode",
    "on_request",
    "parse",
    "trim",
    "error_pattern",
    "envelope",
    "stderr",
    "on_response",
    "on_error",
    "startup_timeout",
    "ready",
    "request_timeout",
    "stop_signal",
    "stop_message",
    "stop_timeout",
    "before_start",
    "after_start",
    "before_stop",
    "after_stop",
    "prelude",
    "url",
    "method",
    "headers",
    "query",
    "body",
    "ok_status",
    "addr",
    "path",
    "env",
    "cwd",
    "cmd",
    "shell",
    "coproc",
    "restart",
    "max_restarts",
    "lazy",
    "external",
];

/// Parse a YAML mapping `Value` into an [`IpcSpec`].
///
/// Validates all known fields and rejects unknown ones with `E010`.
/// `cmd` is required unless `external: true`.
pub fn parse_ipc_spec(value: &Value) -> Result<IpcSpec, Vec<Diagnostic>> {
    let map = match value {
        Value::Object(m) => m,
        _ => {
            return Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: "IPC spec must be a mapping".to_string(),
            }]);
        }
    };

    for key in map.keys() {
        if !KNOWN_FIELDS.contains(&key.as_str()) {
            return Err(vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: format!("unknown IPC spec field '{key}'"),
            }]);
        }
    }

    let runner = map
        .get("runner")
        .map(parse_runner)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcRunner::Process);

    let transport = map
        .get("transport")
        .map(parse_transport)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcTransport::Pipe);

    let protocol = map
        .get("protocol")
        .map(parse_protocol)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcProtocol::Line);

    let mode = map
        .get("mode")
        .map(parse_mode)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcMode::Text);

    let parse = map
        .get("parse")
        .map(parse_parse)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcParse::Yaml);

    let envelope = map
        .get("envelope")
        .map(parse_envelope)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcEnvelope::Payload);

    let stderr = map
        .get("stderr")
        .map(parse_stderr)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcStderr::Ignore);

    let restart = map
        .get("restart")
        .map(parse_restart)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?
        .unwrap_or(IpcRestart::Never);

    #[cfg(feature = "http")]
    let body = map
        .get("body")
        .map(parse_http_body)
        .transpose()
        .map_err(|msg| {
            vec![Diagnostic {
                file: None,
                line: 0,
                col: 0,
                component: None,
                code: E010,
                message: msg,
            }]
        })?;

    let external = optional_bool(map, "external")?;
    let cmd = map.get("cmd").cloned();

    if cmd.is_none() && !external {
        return Err(vec![Diagnostic {
            file: None,
            line: 0,
            col: 0,
            component: None,
            code: E010,
            message: "missing required field 'cmd' (required unless external: true)".to_string(),
        }]);
    }

    let request_template = optional_string(map, "request_template")?;
    let reply_until = optional_string(map, "reply_until")?;
    let on_request = optional_string(map, "on_request")?;
    let error_pattern = optional_string(map, "error_pattern")?;
    let on_response = optional_string(map, "on_response")?;
    let on_error = optional_string(map, "on_error")?;
    let startup_timeout = optional_u64(map, "startup_timeout")?;
    let ready = optional_string(map, "ready")?;
    let request_timeout = optional_u64(map, "request_timeout")?;
    let stop_signal = optional_string(map, "stop_signal")?;
    let stop_message = optional_string(map, "stop_message")?;
    let stop_timeout = optional_u64(map, "stop_timeout")?;
    let before_start = optional_string(map, "before_start")?;
    let after_start = optional_string(map, "after_start")?;
    let before_stop = optional_string(map, "before_stop")?;
    let after_stop = optional_string(map, "after_stop")?;
    let prelude = optional_string(map, "prelude")?;
    #[cfg(feature = "http")]
    let url = optional_string(map, "url")?;
    #[cfg(feature = "http")]
    let method = optional_string(map, "method")?;
    #[cfg(feature = "http")]
    let headers = optional_map(map, "headers")?;
    #[cfg(feature = "http")]
    let query = optional_string_vec(map, "query")?;
    #[cfg(feature = "http")]
    let ok_status = optional_string(map, "ok_status")?;
    let addr = optional_string(map, "addr")?;
    let path = optional_string(map, "path")?;
    let env = optional_map(map, "env")?;
    let cwd = optional_string(map, "cwd")?;
    let shell = optional_bool(map, "shell")?;
    let coproc = optional_bool(map, "coproc")?;
    let max_restarts = optional_u32(map, "max_restarts", 0)?;
    let lazy = optional_bool(map, "lazy")? || !map.contains_key("lazy");

    let trim = optional_bool(map, "trim")? || !map.contains_key("trim");

    Ok(IpcSpec {
        runner,
        transport,
        protocol,
        request_template,
        reply_until,
        mode,
        on_request,
        parse,
        trim,
        error_pattern,
        envelope,
        stderr,
        on_response,
        on_error,
        startup_timeout,
        ready,
        request_timeout,
        stop_signal,
        stop_message,
        stop_timeout,
        before_start,
        after_start,
        before_stop,
        after_stop,
        prelude,
        #[cfg(feature = "http")]
        url,
        #[cfg(feature = "http")]
        method,
        #[cfg(feature = "http")]
        headers,
        #[cfg(feature = "http")]
        query,
        #[cfg(feature = "http")]
        body,
        #[cfg(feature = "http")]
        ok_status,
        addr,
        path,
        env,
        cwd,
        cmd,
        shell,
        coproc,
        restart,
        max_restarts,
        lazy,
        external,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipc_spec_minimal_external() {
        let v = Value::object({
            let mut m = IndexMap::new();
            m.insert("external".to_string(), Value::bool(true));
            m.insert("cmd".to_string(), Value::array(vec![]));
            m
        });
        let spec = parse_ipc_spec(&v).unwrap();
        assert!(spec.external);
        assert_eq!(spec.runner, IpcRunner::Process);
        assert_eq!(spec.transport, IpcTransport::Pipe);
        assert_eq!(spec.protocol, IpcProtocol::Line);
        assert_eq!(spec.mode, IpcMode::Text);
        assert_eq!(spec.parse, IpcParse::Yaml);
        assert!(spec.trim);
        assert!(spec.lazy);
    }

    #[test]
    fn parse_ipc_spec_requires_cmd_when_not_external() {
        let v = Value::object(IndexMap::new());
        let errs = parse_ipc_spec(&v).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, E010);
        assert!(errs[0].message.contains("'cmd'"));
    }

    #[test]
    fn parse_ipc_spec_rejects_unknown_fields() {
        let v = Value::object({
            let mut m = IndexMap::new();
            m.insert("unknown_key".to_string(), Value::string("x"));
            m
        });
        let errs = parse_ipc_spec(&v).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, E010);
        assert!(errs[0].message.contains("unknown_key"));
    }

    #[test]
    fn parse_ipc_spec_not_a_mapping() {
        let v = Value::string("not a map");
        let errs = parse_ipc_spec(&v).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("mapping"));
    }

    #[test]
    fn parse_ipc_spec_full_config() {
        let v = Value::object({
            let mut m = IndexMap::new();
            m.insert("cmd".to_string(), Value::array(vec![Value::string("cat")]));
            m.insert("runner".to_string(), Value::string("external"));
            m.insert("transport".to_string(), Value::string("pipe"));
            m.insert("protocol".to_string(), Value::string("sentinel"));
            m.insert("mode".to_string(), Value::string("json"));
            m.insert("parse".to_string(), Value::string("yaml"));
            m.insert("envelope".to_string(), Value::string("full"));
            m.insert("stderr".to_string(), Value::string("capture"));
            m.insert("restart".to_string(), Value::string("on-failure"));
            m.insert("trim".to_string(), Value::bool(false));
            m.insert("shell".to_string(), Value::bool(true));
            m.insert("lazy".to_string(), Value::bool(false));
            m.insert("max_restarts".to_string(), Value::int(3));
            m.insert("external".to_string(), Value::bool(true));
            m.insert("ready".to_string(), Value::string("READY".to_string()));
            m.insert(
                "request_template".to_string(),
                Value::string("{$0}\n__DONE__\n".to_string()),
            );
            m.insert(
                "reply_until".to_string(),
                Value::string("^__DONE__$".to_string()),
            );
            m.insert(
                "error_pattern".to_string(),
                Value::string("^ERROR".to_string()),
            );
            m.insert("startup_timeout".to_string(), Value::int(5000));
            m.insert("request_timeout".to_string(), Value::int(10000));
            m
        });
        let spec = parse_ipc_spec(&v).unwrap();
        assert!(spec.external);
        assert_eq!(spec.runner, IpcRunner::External);
        assert_eq!(spec.transport, IpcTransport::Pipe);
        assert_eq!(spec.protocol, IpcProtocol::Sentinel);
        assert_eq!(spec.mode, IpcMode::Json);
        assert_eq!(spec.parse, IpcParse::Yaml);
        assert_eq!(spec.envelope, IpcEnvelope::Full);
        assert_eq!(spec.stderr, IpcStderr::Capture);
        assert_eq!(spec.restart, IpcRestart::OnFailure);
        assert!(!spec.trim);
        assert!(spec.shell);
        assert!(!spec.lazy);
        assert_eq!(spec.max_restarts, 3);
        assert_eq!(spec.ready.as_deref(), Some("READY"));
        assert_eq!(spec.request_template.as_deref(), Some("{$0}\n__DONE__\n"));
        assert_eq!(spec.reply_until.as_deref(), Some("^__DONE__$"));
        assert_eq!(spec.error_pattern.as_deref(), Some("^ERROR"));
        assert_eq!(spec.startup_timeout, Some(5000));
        assert_eq!(spec.request_timeout, Some(10000));
    }

    #[test]
    fn parse_ipc_spec_bad_enum_values() {
        let v = Value::object({
            let mut m = IndexMap::new();
            m.insert("cmd".to_string(), Value::array(vec![Value::string("echo")]));
            m.insert("transport".to_string(), Value::string("udp"));
            m
        });
        let errs = parse_ipc_spec(&v).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("transport"));
    }
}
