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

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use indexmap::IndexMap;
use regex::Regex;

use ymx_core::diag::{FileId, E001, E002, E004, E005, E007, E009, E015};
use ymx_core::namespace::{extract_document, DefClass};
use ymx_core::parse::parse_document;

pub use ymx_core;
pub use ymx_core::diag::Diagnostic;
pub use ymx_core::exec::{CommandExecutor, ExecError, ExecOutput};
use ymx_core::ipc::{IpcError, IpcHost, IpcProtocol, IpcRequest, IpcResponse, IpcRestart, IpcSpec};
pub use ymx_core::ir::Value;
pub use ymx_core::project::{Format, Options, Project};

/// Default command executor that shells out to the platform's shell.
///
/// - `"sh"` → `sh -c <command>`
/// - `"pw"` → `pwsh -c <command>` (or `powershell -Command` on Windows)
/// - anything else → [`ExecError::UnknownBackend`]
#[derive(Debug)]
pub struct StdExecutor;

impl CommandExecutor for StdExecutor {
    fn execute(&self, backend: &str, command: &str) -> Result<ExecOutput, ExecError> {
        let mut cmd = match backend {
            "sh" => {
                let mut c = Command::new("sh");
                c.arg("-c").arg(command);
                c
            }
            "pw" => {
                let mut c = if cfg!(windows) {
                    Command::new("powershell")
                } else {
                    Command::new("pwsh")
                };
                if cfg!(windows) {
                    c.arg("-Command").arg(command);
                } else {
                    c.arg("-c").arg(command);
                }
                c
            }
            other => return Err(ExecError::UnknownBackend(other.to_string())),
        };

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| ExecError::SpawnFailed(e.to_string()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }
}

// ---------------------------------------------------------------------------
// StdIpcHost (Task 1.42-4)
// ---------------------------------------------------------------------------

/// Session key for caching IPC processes.
///
/// Key = (project_root, alias, spec_hash). `spec_hash` is a hash of the
/// serialized IpcSpec so that spec changes trigger a new session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    project_root: PathBuf,
    alias: String,
    spec_hash: String,
}

/// Active subprocess session.
#[derive(Debug)]
struct Session {
    child: Option<Child>,
    #[cfg(unix)]
    unix_socket: Option<UnixStream>,
    tcp_socket: Option<TcpStream>,
    spec: IpcSpec,
    dead: bool,
    restart_count: u32,
}

/// Standard stdio-based IPC host for rule-21 external components.
///
/// Manages a cache of subprocess sessions keyed by `(project_root, alias, spec_hash)`.
/// Sessions are restarted on failure when `spec.restart == IpcRestart::OnFailure`.
#[derive(Debug)]
pub struct StdIpcHost {
    sessions: Mutex<HashMap<SessionKey, Session>>,
    executor: Arc<dyn CommandExecutor>,
}

impl StdIpcHost {
    /// Construct a new `StdIpcHost` with the given command executor.
    pub fn new(executor: Arc<dyn CommandExecutor>) -> Self {
        StdIpcHost {
            sessions: Mutex::new(HashMap::new()),
            executor,
        }
    }

    /// Compute a simple hash of the IpcSpec for session-cache keying.
    /// Uses JSON serialization of the cmd field as the primary hash input,
    /// supplemented with other session-affecting fields.
    fn spec_hash(spec: &IpcSpec) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();

        // Hash transport type
        format!("{:?}", spec.transport).hash(&mut h);

        // Hash socket-specific fields (path/addr) for socket transport
        if matches!(spec.transport, ymx_core::ipc::IpcTransport::Socket) {
            if let Some(ref path) = spec.path {
                path.hash(&mut h);
            }
            if let Some(ref addr) = spec.addr {
                addr.hash(&mut h);
            }
        }

        // Hash the cmd value (the primary session identity factor for pipe transport)
        if let Some(ref cmd) = spec.cmd {
            let cmd_json = serde_json::to_string(cmd).unwrap_or_default();
            cmd_json.hash(&mut h);
        }

        // Supplement with other session-affecting fields
        spec.shell.hash(&mut h);
        if let Some(ref cwd) = spec.cwd {
            cwd.hash(&mut h);
        }
        // Hash env as a simple representation (key count and first few keys)
        if let Some(ref env) = spec.env {
            env.len().hash(&mut h);
            for (i, (k, _)) in env.iter().take(5).enumerate() {
                k.hash(&mut h);
                i.hash(&mut h);
            }
        }
        format!("{:?}", spec.protocol).hash(&mut h);
        spec.request_template.hash(&mut h);
        spec.reply_until.hash(&mut h);
        spec.startup_timeout.hash(&mut h);
        spec.ready.hash(&mut h);
        spec.request_timeout.hash(&mut h);
        spec.stop_message.hash(&mut h);
        spec.stop_timeout.hash(&mut h);
        spec.stop_signal.hash(&mut h);

        format!("{:x}", h.finish())
    }

    /// Build a `Command` from `spec.cmd` (list or string form).
    fn build_command(spec: &IpcSpec, project_root: &Path) -> Result<Command, IpcError> {
        let cmd = spec
            .cmd
            .as_ref()
            .ok_or_else(|| IpcError::Custom("missing cmd in IpcSpec".to_string()))?;

        let mut cmd_obj = if spec.shell {
            let mut c = Command::new("sh");
            c.arg("-c");
            match cmd {
                Value::String(s) => {
                    c.arg(s);
                }
                Value::Array(arr) => {
                    let shell_cmd = arr
                        .iter()
                        .map(|v| match v {
                            Value::String(s) => s.as_str(),
                            _ => "",
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    c.arg(shell_cmd);
                }
                _ => {
                    return Err(IpcError::Custom(
                        "cmd must be a string or array when shell: true".to_string(),
                    ))
                }
            };
            c
        } else {
            match cmd {
                Value::Array(arr) => {
                    if arr.is_empty() {
                        return Err(IpcError::Custom("cmd array cannot be empty".to_string()));
                    }
                    let args: Vec<&str> = arr
                        .iter()
                        .filter_map(|v| match v {
                            Value::String(s) => Some(s.as_str()),
                            _ => None,
                        })
                        .collect();
                    if args.is_empty() {
                        return Err(IpcError::Custom(
                            "cmd array must contain only strings".to_string(),
                        ));
                    }
                    let mut c = Command::new(args[0]);
                    for arg in &args[1..] {
                        c.arg(arg);
                    }
                    c
                }
                Value::String(s) => {
                    let mut c = Command::new("sh");
                    c.arg("-c").arg(s);
                    c
                }
                _ => {
                    return Err(IpcError::Custom(
                        "cmd must be a string or array".to_string(),
                    ))
                }
            }
        };

        // Set cwd
        if let Some(ref cwd) = spec.cwd {
            let full_cwd = if Path::new(cwd).is_absolute() {
                PathBuf::from(cwd)
            } else {
                project_root.join(cwd)
            };
            cmd_obj.current_dir(full_cwd);
        } else {
            cmd_obj.current_dir(project_root);
        }

        // Merge env vars
        if let Some(ref env_map) = spec.env {
            for (key, val) in env_map {
                let val_str = match val {
                    Value::String(s) => s.as_str(),
                    _ => continue,
                };
                cmd_obj.env(key, val_str);
            }
        }

        // Set up stdin/stdout/stderr pipes
        cmd_obj.stdin(Stdio::piped());
        cmd_obj.stdout(Stdio::piped());
        match spec.stderr {
            ymx_core::ipc::IpcStderr::Ignore => {
                cmd_obj.stderr(Stdio::null());
            }
            ymx_core::ipc::IpcStderr::Capture => {
                cmd_obj.stderr(Stdio::piped());
            }
            ymx_core::ipc::IpcStderr::Fail => {
                cmd_obj.stderr(Stdio::piped());
            }
        }

        Ok(cmd_obj)
    }

    /// Execute a lifecycle hook via the CommandExecutor.
    fn run_hook(&self, hook: &Option<String>) -> Result<(), IpcError> {
        if let Some(ref cmd) = hook {
            self.executor
                .execute("sh", cmd)
                .map_err(|e| IpcError::HookFailed(e.to_string()))?;
        }
        Ok(())
    }

    /// Spawn a new subprocess session.
    fn spawn_session(
        &self,
        project_root: &Path,
        _alias: &str,
        spec: &IpcSpec,
    ) -> Result<Session, IpcError> {
        // Run before_start hook
        self.run_hook(&spec.before_start)?;

        match spec.transport {
            ymx_core::ipc::IpcTransport::Pipe => {
                let mut cmd = Self::build_command(spec, project_root)?;

                let mut child = cmd
                    .spawn()
                    .map_err(|e| IpcError::SpawnFailed(e.to_string()))?;

                // Wait for startup (ready pattern)
                if let Some(timeout_ms) = spec.startup_timeout {
                    let timeout = Duration::from_millis(timeout_ms);
                    if let Some(ref ready_pattern) = spec.ready {
                        let ready_re = Regex::new(ready_pattern).map_err(|e| {
                            IpcError::FramingError(format!("invalid ready regex: {}", e))
                        })?;

                        let mut stdout = child.stdout.take().map(BufReader::new);
                        let mut stderr = child.stderr.take().map(BufReader::new);

                        let deadline = std::time::Instant::now() + timeout;

                        // Poll stdout/stderr for the ready pattern
                        let mut stdout_buf = String::new();
                        let mut stderr_buf = String::new();

                        loop {
                            if std::time::Instant::now() >= deadline {
                                let _ = child.kill();
                                return Err(IpcError::Timeout(timeout_ms));
                            }

                            // Check if process exited
                            if let Ok(Some(status)) = child.try_wait() {
                                if !status.success() {
                                    return Err(IpcError::Crashed);
                                }
                            }

                            // Check stdout
                            if let Some(ref mut reader) = stdout {
                                let mut line = String::new();
                                match reader.read_line(&mut line) {
                                    Ok(0) => {}
                                    Ok(_) => {
                                        stdout_buf.push_str(&line);
                                        if ready_re.is_match(&stdout_buf) {
                                            break;
                                        }
                                    }
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                    Err(_) => {}
                                }
                            }

                            // Check stderr
                            if let Some(ref mut reader) = stderr {
                                let mut line = String::new();
                                match reader.read_line(&mut line) {
                                    Ok(0) => {}
                                    Ok(_) => {
                                        stderr_buf.push_str(&line);
                                        if ready_re.is_match(&stderr_buf) {
                                            break;
                                        }
                                    }
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                    Err(_) => {}
                                }
                            }

                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                } else if let Some(ref ready_pattern) = spec.ready {
                    // No timeout but has ready pattern - wait indefinitely
                    let ready_re = Regex::new(ready_pattern).map_err(|e| {
                        IpcError::FramingError(format!("invalid ready regex: {}", e))
                    })?;

                    let mut stdout = child.stdout.take().map(BufReader::new);
                    let mut stderr = child.stderr.take().map(BufReader::new);

                    let mut stdout_buf = String::new();
                    let mut stderr_buf = String::new();

                    loop {
                        // Check if process exited
                        if let Ok(Some(status)) = child.try_wait() {
                            if !status.success() {
                                return Err(IpcError::Crashed);
                            }
                        }

                        // Check stdout
                        if let Some(ref mut reader) = stdout {
                            let mut line = String::new();
                            match reader.read_line(&mut line) {
                                Ok(0) => {}
                                Ok(_) => {
                                    stdout_buf.push_str(&line);
                                    if ready_re.is_match(&stdout_buf) {
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(_) => {}
                            }
                        }

                        // Check stderr
                        if let Some(ref mut reader) = stderr {
                            let mut line = String::new();
                            match reader.read_line(&mut line) {
                                Ok(0) => {}
                                Ok(_) => {
                                    stderr_buf.push_str(&line);
                                    if ready_re.is_match(&stderr_buf) {
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(_) => {}
                            }
                        }

                        std::thread::sleep(Duration::from_millis(10));
                    }
                }

                // Run after_start hook (best effort — session continues even on failure)
                self.run_hook(&spec.after_start).ok();

                Ok(Session {
                    child: Some(child),
                    #[cfg(unix)]
                    unix_socket: None,
                    tcp_socket: None,
                    spec: spec.clone(),
                    dead: false,
                    restart_count: 0,
                })
            }
            ymx_core::ipc::IpcTransport::Socket => {
                // Socket transport: connect to existing socket (external: true required)
                // Run after_start hook after successful connection (best effort)
                self.run_hook(&spec.after_start).ok();

                Ok(Session {
                    child: None,
                    #[cfg(unix)]
                    unix_socket: None,
                    tcp_socket: None,
                    spec: spec.clone(),
                    dead: false,
                    restart_count: 0,
                })
            }
            _ => Err(IpcError::DisallowedTransport(format!(
                "{:?}",
                spec.transport
            ))),
        }
    }

    /// Connect to a socket (unix or tcp) for the given spec.
    fn connect_socket(spec: &IpcSpec) -> Result<Session, IpcError> {
        #[cfg(unix)]
        let unix_socket = if let Some(path) = &spec.path {
            let stream = UnixStream::connect(path)
                .map_err(|e| IpcError::SpawnFailed(format!("unix socket connect failed: {}", e)))?;
            Some(stream)
        } else {
            None
        };

        #[cfg(not(unix))]
        let unix_socket: Option<UnixStream> = None;

        let tcp_socket = if let Some(addr) = &spec.addr {
            let stream = TcpStream::connect(addr)
                .map_err(|e| IpcError::SpawnFailed(format!("tcp socket connect failed: {}", e)))?;
            Some(stream)
        } else {
            None
        };

        Ok(Session {
            child: None,
            #[cfg(unix)]
            unix_socket,
            tcp_socket,
            spec: spec.clone(),
            dead: false,
            restart_count: 0,
        })
    }

    /// Stop a session gracefully.
    fn stop_session(&self, session: &mut Session) -> Result<(), IpcError> {
        // Run before_stop hook (best effort — teardown proceeds regardless)
        self.run_hook(&session.spec.before_stop).ok();

        // Send stop_message if provided (only for pipe transport with child)
        if let Some(ref msg) = session.spec.stop_message {
            if let Some(ref mut child) = session.child {
                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(msg.as_bytes());
                    let _ = stdin.flush();
                }
            }
        }

        // Wait for stop_timeout
        let timeout = session
            .spec
            .stop_timeout
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(5));

        let deadline = std::time::Instant::now() + timeout;

        // For pipe transport, wait for child process
        if let Some(ref mut child) = session.child {
            loop {
                if std::time::Instant::now() >= deadline {
                    // Send SIGTERM
                    #[cfg(unix)]
                    {
                        let _ = child.kill();
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = child.kill();
                    }
                    break;
                }

                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        // For socket transport, close the socket connection
        #[cfg(unix)]
        {
            if session.unix_socket.is_some() {
                session.unix_socket = None;
            }
        }
        if session.tcp_socket.is_some() {
            session.tcp_socket = None;
        }

        // Run after_stop hook
        self.run_hook(&session.spec.after_stop)?;

        Ok(())
    }

    /// Interpolate placeholders in a template string.
    ///
    /// `$0` → positional args joined, `$name` → named arg value.
    fn interpolate_template(template: &str, args: &IndexMap<String, Value>) -> String {
        let mut result = template.to_string();

        // Replace $0 with positional args
        let positional: Vec<String> = args
            .values()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Int(n) => n.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".to_string(),
                _ => serde_json::to_string(v).unwrap_or_default(),
            })
            .collect();
        result = result.replace("$0", &positional.join(" "));

        // Replace $name for each named arg
        for (key, val) in args {
            let placeholder = format!("${}", key);
            let replacement = match val {
                Value::String(s) => s.clone(),
                Value::Int(n) => n.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".to_string(),
                _ => serde_json::to_string(val).unwrap_or_default(),
            };
            result = result.replace(&placeholder, &replacement);
        }

        result
    }

    /// Capture any remaining stderr from a child process after protocol execution.
    /// Reads until EOF and returns the captured stderr content.
    fn capture_child_stderr(child: &mut Option<Child>) -> String {
        if let Some(ref mut child) = child {
            if let Some(ref mut stderr) = child.stderr {
                let mut buf = String::new();
                let mut stderr_buf = [0u8; 1024];
                loop {
                    match stderr.read(&mut stderr_buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.push_str(&String::from_utf8_lossy(&stderr_buf[..n]));
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                return buf;
            }
        }
        String::new()
    }

    /// Send a request and read a response using the specified protocol.
    /// Returns (stdout, stderr) tuple.
    fn do_protocol(
        &self,
        session: &mut Session,
        request: &IpcRequest,
    ) -> Result<(String, String), IpcError> {
        let spec = &session.spec;

        match spec.protocol {
            IpcProtocol::Line => {
                // Line protocol: write request, read one line from response
                // If body is pre-built (e.g., mode:json), use it directly; otherwise interpolate template.
                let request_str = if let Some(ref body) = request.body {
                    format!("{}\n", body)
                } else {
                    let template = spec.request_template.as_deref().unwrap_or("{$0}\n");
                    Self::interpolate_template(template, &request.args)
                };

                let timeout = spec
                    .request_timeout
                    .map(Duration::from_millis)
                    .unwrap_or(Duration::from_secs(30));

                let deadline = std::time::Instant::now() + timeout;

                // Pipe transport: use child stdin/stdout
                if let Some(ref mut child) = session.child {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin
                            .write_all(request_str.as_bytes())
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        stdin
                            .flush()
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        drop(stdin);
                    }

                    if let Some(ref mut stdout) = child.stdout {
                        let mut reader = BufReader::new(stdout);
                        let mut line = String::new();

                        loop {
                            if std::time::Instant::now() >= deadline {
                                return Err(IpcError::Timeout(
                                    spec.request_timeout.unwrap_or(30000),
                                ));
                            }

                            match reader.read_line(&mut line) {
                                Ok(0) => return Err(IpcError::Crashed),
                                Ok(_) => {
                                    let trimmed =
                                        line.trim_end_matches('\n').trim_end_matches('\r');
                                    let stderr = Self::capture_child_stderr(&mut session.child);
                                    return Ok((trimmed.to_string(), stderr));
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(10));
                                    continue;
                                }
                                Err(e) => return Err(IpcError::FramingError(e.to_string())),
                            }
                        }
                    }

                    return Err(IpcError::FramingError("no stdout pipe".to_string()));
                }

                // Socket transport: use unix socket
                #[cfg(unix)]
                if let Some(ref mut sock) = session.unix_socket {
                    sock.write_all(request_str.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut reader = BufReader::new(sock);
                    let mut line = String::new();

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                return Ok((trimmed.to_string(), String::new()));
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                }

                // Socket transport: use tcp socket
                if let Some(ref mut sock) = session.tcp_socket {
                    sock.write_all(request_str.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut reader = BufReader::new(sock);
                    let mut line = String::new();

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                return Ok((trimmed.to_string(), String::new()));
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                }

                Err(IpcError::FramingError("no transport available".to_string()))
            }
            IpcProtocol::Sentinel => {
                // Sentinel protocol: write request, read lines until reply_until regex matches
                let template = spec.request_template.as_deref().unwrap_or("{$0}\n");
                let request_str = Self::interpolate_template(template, &request.args);

                let reply_re = spec.reply_until.as_ref().ok_or_else(|| {
                    IpcError::FramingError("reply_until required for sentinel protocol".to_string())
                })?;
                let re = Regex::new(reply_re).map_err(|e| {
                    IpcError::FramingError(format!("invalid reply_until regex: {}", e))
                })?;

                let timeout = spec
                    .request_timeout
                    .map(Duration::from_millis)
                    .unwrap_or(Duration::from_secs(30));

                let deadline = std::time::Instant::now() + timeout;

                // Pipe transport: use child stdin/stdout
                if let Some(ref mut child) = session.child {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin
                            .write_all(request_str.as_bytes())
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        stdin
                            .flush()
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        drop(stdin);
                    }

                    if let Some(ref mut stdout) = child.stdout {
                        let reader = BufReader::new(stdout);
                        let mut lines: Vec<String> = Vec::new();

                        for line in reader.lines() {
                            if std::time::Instant::now() >= deadline {
                                return Err(IpcError::Timeout(
                                    spec.request_timeout.unwrap_or(30000),
                                ));
                            }

                            let line = line.map_err(|e| IpcError::FramingError(e.to_string()))?;
                            if re.is_match(&line) {
                                let stderr = Self::capture_child_stderr(&mut session.child);
                                return Ok((lines.join("\n"), stderr));
                            }
                            lines.push(line);
                        }

                        return Err(IpcError::Crashed);
                    }

                    return Err(IpcError::FramingError("no stdout pipe".to_string()));
                }

                // Socket transport: use unix socket
                #[cfg(unix)]
                if let Some(ref mut sock) = session.unix_socket {
                    sock.write_all(request_str.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut reader = BufReader::new(sock);
                    let mut lines: Vec<String> = Vec::new();

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                if re.is_match(trimmed) {
                                    return Ok((lines.join("\n"), String::new()));
                                }
                                lines.push(trimmed.to_string());
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                }

                // Socket transport: use tcp socket
                if let Some(ref mut sock) = session.tcp_socket {
                    sock.write_all(request_str.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut reader = BufReader::new(sock);
                    let mut lines: Vec<String> = Vec::new();

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                if re.is_match(trimmed) {
                                    return Ok((lines.join("\n"), String::new()));
                                }
                                lines.push(trimmed.to_string());
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                }

                Err(IpcError::FramingError("no transport available".to_string()))
            }
            IpcProtocol::Raw => {
                // Raw protocol: write request, close writer, read to EOF
                let template = spec.request_template.as_deref().unwrap_or("{}\n");
                let request_str = Self::interpolate_template(template, &request.args);

                let timeout = spec
                    .request_timeout
                    .map(Duration::from_millis)
                    .unwrap_or(Duration::from_secs(30));

                let deadline = std::time::Instant::now() + timeout;

                // Pipe transport: use child stdin/stdout
                if let Some(ref mut child) = session.child {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin
                            .write_all(request_str.as_bytes())
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        drop(stdin); // Close stdin
                    }

                    if let Some(ref mut stdout) = child.stdout {
                        let mut output = String::new();
                        let mut buf = [0u8; 1024];

                        loop {
                            if std::time::Instant::now() >= deadline {
                                return Err(IpcError::Timeout(
                                    spec.request_timeout.unwrap_or(30000),
                                ));
                            }

                            match stdout.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    output.push_str(&String::from_utf8_lossy(&buf[..n]));
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(10));
                                    continue;
                                }
                                Err(e) => return Err(IpcError::FramingError(e.to_string())),
                            }
                        }

                        let stderr = Self::capture_child_stderr(&mut session.child);
                        return Ok((output, stderr));
                    }

                    return Err(IpcError::FramingError("no stdout pipe".to_string()));
                }

                // Socket transport: use unix socket
                #[cfg(unix)]
                if let Some(ref mut sock) = session.unix_socket {
                    sock.write_all(request_str.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    // For raw protocol with sockets, we don't close the socket
                    // just read until EOF

                    let mut output = String::new();
                    let mut buf = [0u8; 1024];

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        match sock.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                output.push_str(&String::from_utf8_lossy(&buf[..n]));
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }

                    return Ok((output, String::new()));
                }

                // Socket transport: use tcp socket
                if let Some(ref mut sock) = session.tcp_socket {
                    sock.write_all(request_str.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut output = String::new();
                    let mut buf = [0u8; 1024];

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        match sock.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                output.push_str(&String::from_utf8_lossy(&buf[..n]));
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }

                    return Ok((output, String::new()));
                }

                Err(IpcError::FramingError("no transport available".to_string()))
            }
            IpcProtocol::Json => {
                // JSON protocol: send pre-built JSON body, read one line, parse as JSON.
                let request_body = request.body.as_ref().ok_or_else(|| {
                    IpcError::FramingError("body not set for json protocol".to_string())
                })?;

                let timeout = spec
                    .request_timeout
                    .map(Duration::from_millis)
                    .unwrap_or(Duration::from_secs(30));

                let deadline = std::time::Instant::now() + timeout;

                // Pipe transport: use child stdin/stdout
                if let Some(ref mut child) = session.child {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin
                            .write_all(request_body.as_bytes())
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        stdin
                            .write_all(b"\n")
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        stdin
                            .flush()
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        drop(stdin);
                    }

                    if let Some(ref mut stdout) = child.stdout {
                        let mut reader = BufReader::new(stdout);
                        let mut line = String::new();

                        loop {
                            if std::time::Instant::now() >= deadline {
                                return Err(IpcError::Timeout(
                                    spec.request_timeout.unwrap_or(30000),
                                ));
                            }

                            match reader.read_line(&mut line) {
                                Ok(0) => return Err(IpcError::Crashed),
                                Ok(_) => {
                                    let trimmed =
                                        line.trim_end_matches('\n').trim_end_matches('\r');
                                    let stderr = Self::capture_child_stderr(&mut session.child);
                                    return Ok((trimmed.to_string(), stderr));
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(10));
                                    continue;
                                }
                                Err(e) => return Err(IpcError::FramingError(e.to_string())),
                            }
                        }
                    }

                    return Err(IpcError::FramingError("no stdout pipe".to_string()));
                }

                // Socket transport: use unix socket
                #[cfg(unix)]
                if let Some(ref mut sock) = session.unix_socket {
                    sock.write_all(request_body.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.write_all(b"\n")
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut reader = BufReader::new(sock);
                    let mut line = String::new();

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                return Ok((trimmed.to_string(), String::new()));
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                }

                // Socket transport: use tcp socket
                if let Some(ref mut sock) = session.tcp_socket {
                    sock.write_all(request_body.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.write_all(b"\n")
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let mut reader = BufReader::new(sock);
                    let mut line = String::new();

                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }

                        match reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                return Ok((trimmed.to_string(), String::new()));
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                }

                Err(IpcError::FramingError("no transport available".to_string()))
            }
            IpcProtocol::Jsonrpc => {
                // JSON-RPC 2.0 protocol: send pre-built JSON-RPC request, read response, extract result.
                let request_body = request.body.as_ref().ok_or_else(|| {
                    IpcError::FramingError("body not set for jsonrpc protocol".to_string())
                })?;

                let timeout = spec
                    .request_timeout
                    .map(Duration::from_millis)
                    .unwrap_or(Duration::from_secs(30));

                let deadline = std::time::Instant::now() + timeout;

                // Helper to read one line from a reader
                let read_response_line = |reader: &mut dyn Read| -> Result<String, IpcError> {
                    let mut buf_reader = BufReader::new(reader);
                    let mut line = String::new();
                    loop {
                        if std::time::Instant::now() >= deadline {
                            return Err(IpcError::Timeout(spec.request_timeout.unwrap_or(30000)));
                        }
                        match buf_reader.read_line(&mut line) {
                            Ok(0) => {
                                return Err(IpcError::FramingError("socket closed".to_string()))
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                                return Ok(trimmed.to_string());
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                            Err(e) => return Err(IpcError::FramingError(e.to_string())),
                        }
                    }
                };

                // Pipe transport: use child stdin/stdout
                if let Some(ref mut child) = session.child {
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin
                            .write_all(request_body.as_bytes())
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        stdin
                            .write_all(b"\n")
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        stdin
                            .flush()
                            .map_err(|e| IpcError::FramingError(e.to_string()))?;
                        drop(stdin);
                    }

                    if let Some(ref mut stdout) = child.stdout {
                        let response_line = read_response_line(stdout)?;
                        let result = Self::parse_jsonrpc_response(&response_line)?;
                        let stderr = Self::capture_child_stderr(&mut session.child);
                        return Ok((result, stderr));
                    }

                    return Err(IpcError::FramingError("no stdout pipe".to_string()));
                }

                // Socket transport: use unix socket
                #[cfg(unix)]
                if let Some(ref mut sock) = session.unix_socket {
                    sock.write_all(request_body.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.write_all(b"\n")
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let response_line = read_response_line(sock)?;
                    let result = Self::parse_jsonrpc_response(&response_line)?;
                    return Ok((result, String::new()));
                }

                // Socket transport: use tcp socket
                if let Some(ref mut sock) = session.tcp_socket {
                    sock.write_all(request_body.as_bytes())
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.write_all(b"\n")
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;
                    sock.flush()
                        .map_err(|e| IpcError::FramingError(e.to_string()))?;

                    let response_line = read_response_line(sock)?;
                    let result = Self::parse_jsonrpc_response(&response_line)?;
                    return Ok((result, String::new()));
                }

                Err(IpcError::FramingError("no transport available".to_string()))
            }
        }
    }

    /// Parse a JSON-RPC 2.0 response and extract the result or error.
    fn parse_jsonrpc_response(response_line: &str) -> Result<String, IpcError> {
        let response: serde_json::Value = serde_json::from_str(response_line)
            .map_err(|e| IpcError::FramingError(format!("JSON-RPC response parse error: {}", e)))?;

        // Check for JSON-RPC error response
        if let Some(error_obj) = response.get("error") {
            if let Some(msg) = error_obj.get("message").and_then(|m| m.as_str()) {
                let code = error_obj
                    .get("code")
                    .and_then(|c| c.as_i64())
                    .unwrap_or(-32768);
                return Err(IpcError::FramingError(format!(
                    "JSON-RPC error {}: {}",
                    code, msg
                )));
            }
            return Err(IpcError::FramingError(
                "JSON-RPC error response without message".to_string(),
            ));
        }

        // Extract result
        let result = response.get("result").ok_or_else(|| {
            IpcError::FramingError("JSON-RPC response missing result field".to_string())
        })?;

        // Serialize result back to string for handle_ipc_response to parse
        serde_json::to_string(result).map_err(|e| {
            IpcError::FramingError(format!("JSON-RPC result serialization error: {}", e))
        })
    }

    /// Make an HTTP request (rule-21 HTTP transport).
    ///
    /// Stateless: no session management, no lifecycle hooks, no restart policy.
    #[cfg(feature = "http")]
    fn call_http(&self, spec: &IpcSpec, request: &IpcRequest) -> Result<String, IpcError> {
        use std::time::Duration;

        // URL is required for HTTP transport
        let url = spec
            .url
            .as_ref()
            .ok_or_else(|| IpcError::Custom("url is required for HTTP transport".to_string()))?;

        // Interpolate URL with args ($0, $name placeholders)
        let interpolated_url = Self::interpolate_template(url, &request.args);

        // Determine HTTP method (default POST)
        let method = spec.method.as_deref().unwrap_or("POST");
        let is_get_or_head = method == "GET" || method == "HEAD";

        // Collect query param names
        let query_param_names: Vec<&str> = spec
            .query
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        // Separate query args from body args
        let query_args: IndexMap<String, &Value> = request
            .args
            .iter()
            .filter(|(k, _)| query_param_names.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v))
            .collect();

        let body_args: IndexMap<String, &Value> = request
            .args
            .iter()
            .filter(|(k, _)| !query_param_names.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v))
            .collect();

        // Build URL with query string
        let final_url = if !query_args.is_empty() {
            let query_string: String = query_args
                .iter()
                .map(|(k, v)| {
                    let rendered = Self::render_value_for_http(v, &spec.mode);
                    format!(
                        "{}={}",
                        urlencoding::encode(k),
                        urlencoding::encode(&rendered)
                    )
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{}?{}", interpolated_url, query_string)
        } else {
            interpolated_url.clone()
        };

        // Build request body based on body shaping
        let body: Option<String> = if is_get_or_head {
            // GET/HEAD: no body regardless of body setting
            None
        } else {
            match spec
                .body
                .as_ref()
                .unwrap_or(&ymx_core::ipc::IpcHttpBody::All)
            {
                ymx_core::ipc::IpcHttpBody::Off => None,
                ymx_core::ipc::IpcHttpBody::All => {
                    if body_args.is_empty() {
                        None
                    } else {
                        Some(Self::render_args_for_body(&body_args, &spec.mode)?)
                    }
                }
                ymx_core::ipc::IpcHttpBody::Positional => {
                    // Only $0 (first positional arg, which is the first arg by insertion order)
                    let first_val = body_args.values().next();
                    first_val.map(|v| Self::render_value_for_http(v, &spec.mode))
                }
                ymx_core::ipc::IpcHttpBody::Named(name) => body_args
                    .get(name)
                    .map(|v| Self::render_value_for_http(v, &spec.mode)),
            }
        };

        // Build headers
        let mut req = ureq::request(method, &final_url);

        // Set Content-Type header based on mode if body is present
        if body.is_some() {
            let content_type = match spec.mode {
                ymx_core::ipc::IpcMode::Json => "application/json",
                ymx_core::ipc::IpcMode::Text => "text/plain",
            };
            req = req.set("Content-Type", content_type);
        }

        // Apply user-specified headers
        if let Some(ref headers) = spec.headers {
            for (key, val) in headers {
                let value = match val {
                    Value::String(s) => s.clone(),
                    _ => serde_json::to_string(val).unwrap_or_default(),
                };
                req = req.set(key, &value);
            }
        }

        // Set timeout
        if let Some(timeout_ms) = spec.request_timeout {
            req = req.timeout(Duration::from_millis(timeout_ms));
        }

        // Send request
        let response = if let Some(ref body_str) = body {
            req.send_string(body_str)
                .map_err(|e| IpcError::SpawnFailed(format!("HTTP request failed: {}", e)))?
        } else {
            req.call()
                .map_err(|e| IpcError::SpawnFailed(format!("HTTP request failed: {}", e)))?
        };

        // Check status code
        let status = response.status();
        let ok_spec = spec.ok_status.as_deref().unwrap_or("2xx");
        if !Self::check_ok_status(status, ok_spec) {
            let body_text = response.into_string().map_err(|e| {
                IpcError::FramingError(format!("Failed to read response body: {}", e))
            })?;
            return Err(IpcError::StatusCode(status, body_text));
        }

        // Read response body
        let response_body = response
            .into_string()
            .map_err(|e| IpcError::FramingError(format!("Failed to read response body: {}", e)))?;

        Ok(response_body)
    }

    /// Render a single Value for HTTP (URL query or body).
    #[cfg(feature = "http")]
    fn render_value_for_http(value: &Value, mode: &ymx_core::ipc::IpcMode) -> String {
        match value {
            Value::String(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            _ => match mode {
                ymx_core::ipc::IpcMode::Json => serde_json::to_string(value).unwrap_or_default(),
                ymx_core::ipc::IpcMode::Text => serde_json::to_string(value).unwrap_or_default(),
            },
        }
    }

    /// Render args for HTTP request body.
    #[cfg(feature = "http")]
    fn render_args_for_body(
        args: &IndexMap<String, &Value>,
        mode: &ymx_core::ipc::IpcMode,
    ) -> Result<String, IpcError> {
        match mode {
            ymx_core::ipc::IpcMode::Text => {
                // Text mode: join all values with space
                let rendered: Vec<String> = args
                    .values()
                    .map(|v| Self::render_value_for_http(v, mode))
                    .collect();
                Ok(rendered.join(" "))
            }
            ymx_core::ipc::IpcMode::Json => {
                // JSON mode: render as JSON object
                let obj: IndexMap<String, Value> = args
                    .iter()
                    .map(|(k, v)| {
                        let rendered: Value = match (*v).clone() {
                            Value::String(s) => Value::String(s),
                            Value::Int(n) => Value::Int(n),
                            Value::Float(f) => Value::Float(f),
                            Value::Bool(b) => Value::Bool(b),
                            Value::Null => Value::Null,
                            other => other.clone(),
                        };
                        (k.clone(), rendered)
                    })
                    .collect();
                serde_json::to_string(&obj)
                    .map_err(|e| IpcError::Custom(format!("JSON render error: {}", e)))
            }
        }
    }

    /// Check if HTTP status code matches the ok specification.
    #[cfg(feature = "http")]
    fn check_ok_status(status: u16, ok_spec: &str) -> bool {
        match ok_spec {
            "2xx" => (200..=299).contains(&status),
            "3xx" => (300..=399).contains(&status),
            "4xx" => (400..=499).contains(&status),
            "5xx" => (500..=599).contains(&status),
            s => {
                // Try to parse as a specific status code
                if let Ok(code) = s.parse::<u16>() {
                    status == code
                } else {
                    // Default to 2xx
                    (200..=299).contains(&status)
                }
            }
        }
    }
}

impl IpcHost for StdIpcHost {
    fn call(
        &self,
        name: &str,
        spec: &IpcSpec,
        request: IpcRequest,
    ) -> Result<IpcResponse, IpcError> {
        // Handle HTTP transport directly (stateless, no session management)
        #[cfg(feature = "http")]
        if matches!(spec.transport, ymx_core::ipc::IpcTransport::Http) {
            let output = self.call_http(spec, &request)?;
            return Ok(IpcResponse {
                stdout: output,
                stderr: String::new(),
                status: None,
            });
        }

        // Get project root - we use "." as default since we don't have access to it here
        // The actual project root would be passed during construction or via Options
        let project_root = PathBuf::from(".");

        let key = SessionKey {
            project_root: project_root.clone(),
            alias: name.to_string(),
            spec_hash: Self::spec_hash(spec),
        };

        // Get or create session
        let mut sessions = self.sessions.lock().unwrap();

        // Try to use existing session
        if let Some(ref mut s) = sessions.get_mut(&key).filter(|s| !s.dead) {
            // For socket transport, check if socket is still connected
            #[cfg(unix)]
            let socket_dead = s
                .unix_socket
                .as_ref()
                .map(|s| s.peer_addr().is_err())
                .unwrap_or(false);
            #[cfg(not(unix))]
            let socket_dead = s
                .tcp_socket
                .as_ref()
                .map(|s| s.peer_addr().is_err())
                .unwrap_or(false);

            if !socket_dead {
                match self.do_protocol(s, &request) {
                    Ok((stdout, stderr)) => {
                        if spec.transport == ymx_core::ipc::IpcTransport::Pipe
                            && (spec.protocol == ymx_core::ipc::IpcProtocol::Line
                                || spec.protocol == ymx_core::ipc::IpcProtocol::Sentinel)
                        {
                            s.dead = true;
                        }
                        return Ok(IpcResponse {
                            stdout,
                            stderr,
                            status: None,
                        });
                    }
                    Err(e) => {
                        // Mark dead and respawn if OnFailure
                        s.dead = true;
                        if spec.restart != IpcRestart::OnFailure {
                            return Err(e);
                        }
                        // Fall through to respawn
                    }
                }
            } else {
                s.dead = true;
                if spec.restart != IpcRestart::OnFailure {
                    return Err(IpcError::FramingError("socket closed".to_string()));
                }
            }
        }

        // No session exists yet → always spawn/connect (first call).
        // Session exists but is dead → respawn when restart == OnFailure or Line+pipe (stdin consumed).
        let has_dead_session = sessions.contains_key(&key);
        let line_pipe = spec.transport == ymx_core::ipc::IpcTransport::Pipe
            && spec.protocol == ymx_core::ipc::IpcProtocol::Line;
        let sentinel_pipe = spec.transport == ymx_core::ipc::IpcTransport::Pipe
            && spec.protocol == ymx_core::ipc::IpcProtocol::Sentinel;
        let stdin_consumed_pipe = line_pipe || sentinel_pipe;
        if !has_dead_session || spec.restart == IpcRestart::OnFailure || stdin_consumed_pipe {
            // Check max_restarts when respawning a dead session (but not for stdin-consumed pipe
            // protocols, where respawning is inherent to the protocol since stdin is consumed per call).
            let restart_count = if has_dead_session {
                if let Some(dead_session) = sessions.get(&key) {
                    if !stdin_consumed_pipe && dead_session.restart_count >= spec.max_restarts {
                        return Err(IpcError::Custom("max restarts exceeded".to_string()));
                    }
                    dead_session.restart_count + 1
                } else {
                    1
                }
            } else {
                0
            };

            let mut new_session = match spec.transport {
                ymx_core::ipc::IpcTransport::Socket => {
                    // For socket transport, connect to the socket
                    Self::connect_socket(spec)?
                }
                ymx_core::ipc::IpcTransport::Pipe => {
                    self.spawn_session(&project_root, name, spec)?
                }
                _ => {
                    return Err(IpcError::DisallowedTransport(format!(
                        "{:?}",
                        spec.transport
                    )))
                }
            };

            // Set the restart count for the new session
            new_session.restart_count = restart_count;

            let output = self.do_protocol(&mut new_session, &request);

            if stdin_consumed_pipe && output.is_ok() {
                new_session.dead = true;
            }

            // Store the session (even if the call failed)
            sessions.insert(key, new_session);

            output.map(|(stdout, stderr)| IpcResponse {
                stdout,
                stderr,
                status: None,
            })
        } else {
            Err(IpcError::Crashed)
        }
    }

    fn shutdown(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        for (_key, mut session) in sessions.drain() {
            let _ = self.stop_session(&mut session);
        }
    }
}

impl Drop for StdIpcHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The three forms of `_use` values (parsed from `MetaValue.value`).
#[derive(Debug, Clone)]
enum RawUse {
    /// `_use: *` → recursive wildcard walk of the entry's directory
    WildcardAll,
    /// `_use: {"*": "foo"}` → import all public components from `foo.yml`
    WildcardFile(String),
    /// `_use: {x: "foo.bar", ...}` → named imports
    NamedImports(Vec<(String, String, String)>), // (alias, file_path, component)
    /// `_use: {alias: {...}, ...}` → IPC declarations (mapping RHS)
    IpcDeclarations(Vec<(String, ymx_core::ir::Value)>), // (alias, raw IPC spec value)
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
            // OR IPC declarations: {alias: {...}, ...}
            let mut named = Vec::new();
            let mut ipc_decls = Vec::new();
            for (alias, v) in m.iter() {
                if alias == "*" {
                    // `*` key with non-string value — treated as IPC if object, skipped otherwise
                    if let ymx_core::ir::Value::Object(_) = v {
                        // This is weird: {"*": {...}} - skip for now
                    }
                    continue;
                }
                if let ymx_core::ir::Value::String(rhs) = v {
                    // RHS is "file.component" — file import
                    let parts: Vec<&str> = rhs.split('.').collect();
                    if parts.len() >= 2 {
                        let component = parts.last().unwrap();
                        let file_path = parts[..parts.len() - 1].join(".");
                        named.push((alias.clone(), file_path, component.to_string()));
                    } else {
                        return None; // invalid RHS format
                    }
                } else if let ymx_core::ir::Value::Object(_) = v {
                    // RHS is a mapping — IPC declaration
                    ipc_decls.push((alias.clone(), v.clone()));
                } else {
                    // Non-string, non-object RHS is invalid
                    return None;
                }
            }
            // If we have both file imports and IPC declarations, prefer file imports
            // (IPC declarations are handled separately via IpcDeclarations variant)
            if !ipc_decls.is_empty() && !named.is_empty() {
                // Mixed: file imports + IPC declarations. We handle file imports only here,
                // and IPC declarations via IpcDeclarations. Return file imports.
                // Actually, we should handle both - let's check if this case matters.
                // For now, return file imports and handle IPC separately.
            }
            if !ipc_decls.is_empty() && named.is_empty() {
                return Some(RawUse::IpcDeclarations(ipc_decls));
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
                stem,
                stem,
                stem,
                dir.display()
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
                stem,
                stem,
                stem,
                dir.display()
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
        _is_entry: bool,
        visited: &mut HashSet<PathBuf>,
        stack: &mut Vec<PathBuf>,
        result: &mut Vec<PathBuf>,
        diags: &mut Vec<Diagnostic>,
    ) {
        // Canonicalize path
        let canonical = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.to_path_buf());

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
        true, // _is_entry
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
        // Named imports via _use: alias -> (target_file_path, target_component)
        // These are populated after the first pass
        use_imports: Option<Vec<(String, PathBuf, String)>>,
        // IPC aliases declared in this file's _use block: (alias, IpcSpec)
        ipc_aliases: Vec<(String, ymx_core::ipc::IpcSpec)>,
    }

    let mut file_data_map: std::collections::HashMap<PathBuf, FileData> =
        std::collections::HashMap::new();

    // First pass: parse all files and extract their data (minus _use imports)
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
                use_imports: None,
                ipc_aliases: Vec::new(),
            },
        );
    }

    // Second pass: extract _use imports for each file
    for path in &file_paths {
        let contents = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let node = match parse_document(&contents) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let extract = extract_document(FileId(0), &node);
        let raw_use = extract.meta_use.and_then(|mv| parse_raw_use(&mv.value));

        if let Some(RawUse::NamedImports(ref imports)) = raw_use {
            let resolved_imports: Vec<(String, PathBuf, String)> = imports
                .iter()
                .filter_map(|(alias, file_path_str, component)| {
                    resolve_file_stem(file_path_str, &project_root)
                        .ok()
                        .map(|target_path| (alias.clone(), target_path, component.clone()))
                })
                .collect();

            if let Some(fd) = file_data_map.get_mut(path) {
                fd.use_imports = Some(resolved_imports);
            }
        }

        // Handle IPC declarations (mapping RHS in _use entries)
        if let Some(RawUse::IpcDeclarations(decls)) = raw_use {
            if let Some(fd) = file_data_map.get_mut(path) {
                for (alias, raw_spec) in decls {
                    match ymx_core::ipc::parse_ipc_spec(&raw_spec) {
                        Ok(spec) => {
                            fd.ipc_aliases.push((alias, spec));
                        }
                        Err(err_diags) => {
                            diags.extend(err_diags);
                        }
                    }
                }
            }
        }
    }

    // Helper to resolve a component through the _use chain (returns (file_path, component))
    // If the component is directly defined, returns (file_path, component)
    // If the component is imported via _use, follows the chain recursively
    fn resolve_through_use_chain(
        file_path: &Path,
        component: &str,
        file_data_map: &std::collections::HashMap<PathBuf, FileData>,
    ) -> Option<(PathBuf, String)> {
        let fd = file_data_map.get(file_path)?;
        // First check if component is directly defined
        if fd.defs.iter().any(|d| d.full_name == component) {
            return Some((file_path.to_path_buf(), component.to_string()));
        }
        // Then check file-scoped (shouldn't be importable, but check anyway)
        if fd.file_scoped_defs.iter().any(|d| d.full_name == component) {
            return None; // file-scoped, not importable
        }
        // Follow _use imports
        if let Some(ref imports) = fd.use_imports {
            for (_alias, target_path, target_comp) in imports {
                if let Some(result) =
                    resolve_through_use_chain(target_path, target_comp, file_data_map)
                {
                    return Some(result);
                }
            }
        }
        None
    }

    // Determine the entry file's _use directive
    // This determines which components are imported into the global namespace
    #[derive(Debug)]
    enum EntryImport {
        WildcardAll,  // _use: * or no _use — import all from all files
        WildcardFile, // _use: {"*": "stem"} — import all from specific file
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
        Some(RawUse::IpcDeclarations(_)) => {
            // Entry file has only IPC declarations (no file imports)
            // All components are loaded, but only entry file's IPC aliases are stored
            EntryImport::WildcardAll
        }
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
                    // First check if component is directly defined in target file
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
                        continue;
                    }

                    // Resolve component through _use chain (transitive re-export support)
                    let resolved =
                        resolve_through_use_chain(&target_path, &component, &file_data_map);

                    match resolved {
                        Some((_, _)) => {
                            // Found the component (directly or via _use chain)
                            // For the validated entry, use the original target_path and component
                            // (the actual resolution happens at compile time)
                            validated.push((alias, target_path, component));
                        }
                        None => {
                            // Not found - could be file-scoped or not defined
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
                        }
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
        let file_id = FileId(project.files.iter().position(|p| p == path).unwrap() as u32);

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
            // Resolve through _use chain to find the actual definition
            let resolved = resolve_through_use_chain(target_path, component, &file_data_map);
            if let Some((final_path, final_comp)) = resolved {
                if let Some(target_data) = file_data_map.get(&final_path) {
                    if let Some(def) = target_data.defs.iter().find(|d| d.full_name == final_comp) {
                        let mut aliased_def = def.clone();
                        aliased_def.full_name = alias.clone();
                        if let Err(dup) = project.namespaces.register("", aliased_def) {
                            diags.push(dup.into_diagnostic(final_path.clone()));
                        }
                    }
                }
            }
        }
    }

    // Collect IPC aliases into project.ipc
    // Determine which files contribute IPC aliases based on entry_import
    let ipc_contributing_files: Vec<PathBuf> = match &entry_import {
        EntryImport::WildcardAll | EntryImport::WildcardFile => {
            // All files in the graph contribute their IPC aliases
            file_paths.clone()
        }
        EntryImport::NamedImports(_) => {
            // Only the entry file contributes its IPC aliases (direct declarations)
            vec![entry_file.clone()]
        }
    };

    // Helper to check if an alias name is reserved
    fn is_reserved_ipc_name(alias: &str) -> Option<(&'static str, &'static str)> {
        // E015: starts with $_
        if alias.starts_with("$_") {
            return Some((E015, "IPC alias starts with `$_`"));
        }
        // E007: reserved builtin names (map, reduce, merge, sh, pw) and $-prefixed variants
        let effective = alias.trim_start_matches('$');
        if effective == "map"
            || effective == "reduce"
            || effective == "merge"
            || effective == "sh"
            || effective == "pw"
        {
            return Some((E007, "IPC alias is a reserved builtin name"));
        }
        None
    }

    for file_path in &ipc_contributing_files {
        if let Some(fd) = file_data_map.get(file_path) {
            for (alias, spec) in &fd.ipc_aliases {
                // E007 / E015 reserved-name checks
                if let Some((code, msg)) = is_reserved_ipc_name(alias) {
                    diags.push(Diagnostic {
                        file: Some(file_path.clone()),
                        line: 1,
                        col: 1,
                        component: Some(alias.clone()),
                        code,
                        message: msg.to_string(),
                    });
                    continue;
                }

                // E004: check if alias already exists as a component in the global namespace
                if project.namespaces.get("", alias).is_some() {
                    diags.push(Diagnostic {
                        file: Some(file_path.clone()),
                        line: 1,
                        col: 1,
                        component: Some(alias.clone()),
                        code: E004,
                        message: format!(
                            "IPC alias `{}` conflicts with an existing component in the global namespace",
                            alias
                        ),
                    });
                    continue;
                }

                // E004: check if alias already exists in project.ipc (duplicate)
                if project.ipc.contains_key(alias) {
                    diags.push(Diagnostic {
                        file: Some(file_path.clone()),
                        line: 1,
                        col: 1,
                        component: Some(alias.clone()),
                        code: E004,
                        message: format!("duplicate IPC alias `{}` in the same namespace", alias),
                    });
                    continue;
                }

                project.ipc.insert(alias.clone(), spec.clone());
            }
        }
    }

    // Transitive re-export: for wildcard imports, also pull in IPC aliases from
    // files imported via _use chains (not just direct wildcard targets)
    if matches!(
        entry_import,
        EntryImport::WildcardAll | EntryImport::WildcardFile
    ) {
        // Collect all IPC aliases from files reachable via _use chains
        fn collect_transitive_ipc_aliases(
            file_path: &Path,
            file_data_map: &std::collections::HashMap<PathBuf, FileData>,
            visited: &mut HashSet<PathBuf>,
            collected: &mut Vec<(String, ymx_core::ipc::IpcSpec)>,
        ) {
            let canonical = file_path
                .canonicalize()
                .unwrap_or_else(|_| file_path.to_path_buf());
            if visited.contains(&canonical) {
                return;
            }
            visited.insert(canonical.clone());

            if let Some(fd) = file_data_map.get(file_path) {
                for (alias, spec) in &fd.ipc_aliases {
                    if !collected.iter().any(|(a, _)| a == alias) {
                        collected.push((alias.clone(), spec.clone()));
                    }
                }
                // Follow _use imports recursively
                if let Some(ref imports) = fd.use_imports {
                    for (_alias, target_path, _component) in imports {
                        collect_transitive_ipc_aliases(
                            target_path,
                            file_data_map,
                            visited,
                            collected,
                        );
                    }
                }
            }
        }

        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut transitive_ipc: Vec<(String, ymx_core::ipc::IpcSpec)> = Vec::new();
        for file_path in &ipc_contributing_files {
            collect_transitive_ipc_aliases(
                file_path,
                &file_data_map,
                &mut visited,
                &mut transitive_ipc,
            );
        }

        // Add transitive IPC aliases to project.ipc (with reserved-name and E004 checks)
        for (alias, spec) in transitive_ipc {
            if project.ipc.contains_key(&alias) {
                continue; // already added via direct declaration
            }

            // E007 / E015 reserved-name checks
            if let Some((code, msg)) = is_reserved_ipc_name(&alias) {
                diags.push(Diagnostic {
                    file: Some(entry_file.clone()),
                    line: 1,
                    col: 1,
                    component: Some(alias.clone()),
                    code,
                    message: msg.to_string(),
                });
                continue;
            }

            // E004: check if alias already exists as a component
            if project.namespaces.get("", &alias).is_some() {
                diags.push(Diagnostic {
                    file: Some(entry_file.clone()),
                    line: 1,
                    col: 1,
                    component: Some(alias.clone()),
                    code: E004,
                    message: format!(
                        "IPC alias `{}` conflicts with an existing component in the global namespace",
                        alias
                    ),
                });
                continue;
            }

            project.ipc.insert(alias, spec);
        }
    }

    if diags.is_empty() {
        Ok(project)
    } else {
        Err(diags)
    }
}

pub fn load_project_with_override(
    root: &Path,
    override_yaml: Option<&str>,
) -> Result<Project, Vec<Diagnostic>> {
    match override_yaml {
        None => load_project(root),
        Some(yaml_str) => {
            let mut project = load_project(root)?;

            let node = match parse_document(yaml_str) {
                Ok(n) => n,
                Err(parse_err) => {
                    let synthetic = root.join(".ymx-override");
                    return Err(vec![parse_err.into_diagnostic(synthetic)]);
                }
            };

            let synthetic_path = root.join(".ymx-override");
            let file_id = FileId(project.files.len() as u32);
            project.files.push(synthetic_path.clone());

            let extract = extract_document(file_id, &node);

            for def in extract.defs {
                project.namespaces.register_override("", def);
            }

            Ok(project)
        }
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
        dir.write("main.yml", "_use: \"*\"\nmain: 1\n");
        dir.write("a.yml", "a: 2\n");
        dir.write("subdir/b.yml", "b: 3\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

        // All files in the same directory tree should be loaded
        assert!(
            project.namespaces.get("", "a").is_some(),
            "a should be global"
        );
        assert!(
            project.namespaces.get("", "main").is_some(),
            "main should be global"
        );
        assert!(
            project.namespaces.get("", "b").is_some(),
            "b should be global (from subdir)"
        );
    }

    #[test]
    fn use_wildcard_file_imports_all_public_components() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": foo\nfoo: 1\na: 2\n");
        dir.write("foo.yml", "x: 10\ny: 20\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

        assert!(project.namespaces.get("", "x").is_some(), "x imported");
        assert!(project.namespaces.get("", "y").is_some(), "y imported");
        assert!(project.namespaces.get("", "a").is_some(), "a still global");
    }

    #[test]
    fn use_named_import() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  sum: foo.bar\nfoo: 1\n");
        dir.write("foo.yml", "bar: 42\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

        assert!(
            project.namespaces.get("", "sum").is_some(),
            "sum registered"
        );
        let sum_def = project.namespaces.get("", "sum").unwrap();
        assert_eq!(sum_def.file, file_id_of(&project, "foo.yml"));
    }

    #[test]
    fn use_named_import_missing_component_is_e002() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  x: foo.bar\nfoo: 1\n");
        dir.write("foo.yml", "baz: 42\n"); // no `bar` component

        let err =
            load_project(&dir.path().join("main.yml")).expect_err("missing component is E002");
        assert!(
            err.iter().any(|d| d.code == E002),
            "E002 for missing component"
        );
    }

    #[test]
    fn use_named_import_file_scoped_is_e005() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  x: foo._bar\nfoo: 1\n");
        dir.write("foo.yml", "_bar: 42\n"); // file-scoped

        let err =
            load_project(&dir.path().join("main.yml")).expect_err("file-scoped import is E005");
        assert!(err.iter().any(|d| d.code == E005), "E005 for file-scoped");
    }

    #[test]
    fn use_cycle_is_e001() {
        let dir = TempDir::new();
        dir.write("a.yml", "_use:\n  \"*\": b\nmain: 1\n");
        dir.write("b.yml", "_use:\n  \"*\": a\nx: 2\n");

        let err = load_project(&dir.path().join("a.yml")).expect_err("cycle is E001");
        assert!(err.iter().any(|d| d.code == E001), "E001 for cycle");
    }

    #[test]
    fn use_transitive_imports() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": a\nmain: 1\n");
        dir.write("a.yml", "_use:\n  \"*\": b\n");
        dir.write("b.yml", "x: 42\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

        assert!(
            project.namespaces.get("", "x").is_some(),
            "x from transitive b"
        );
    }

    #[test]
    fn use_ambiguous_stem_is_e009() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": foo\nmain: 1\n");
        dir.write("foo.yml", "x: 1\n");
        dir.write("foo.yaml", "y: 2\n");

        let err = load_project(&dir.path().join("main.yml")).expect_err("ambiguous stem is E009");
        assert!(err.iter().any(|d| d.code == E009), "E009 for ambiguity");
    }

    #[test]
    fn use_missing_file_is_e009() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": nonexistent\nmain: 1\n");

        let err = load_project(&dir.path().join("main.yml")).expect_err("missing file is E009");
        assert!(err.iter().any(|d| d.code == E009), "E009 for missing file");
    }

    #[test]
    fn use_directory_entry_loads_main_only() {
        // Passing a directory finds main.yml (no implicit _use: *)
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");
        dir.write("other.yml", "other: 2\n");

        let project = load_project(dir.path()).expect("loads via dir");
        assert!(project.namespaces.get("", "main").is_some());
        assert!(
            project.namespaces.get("", "other").is_none(),
            "other not loaded (no implicit wildcard)"
        );
    }

    #[test]
    fn use_all_public_components_land_in_global_namespace() {
        let dir = TempDir::new();
        dir.write("main.yml", "_use:\n  \"*\": subdir.lib\nmain: 1\n");
        dir.write("subdir/lib.yml", "x: 10\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

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

        let err =
            load_project(&dir.path().join("main.yml")).expect_err("duplicate in global namespace");
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
        assert_eq!(project.namespaces.get("", "main").unwrap().file, main_id);
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
        dir.write(
            "a.yml",
            "_use: \"*\"\n_ymx:\n  v: 1\n_test:\n  t: a\na: 0\n",
        );
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
        dir.write("main.yml", "_use: \"*\"\nmain: 1\n");
        dir.write("notes.txt", "not yaml\n");
        dir.write("README.md", "# readme\n");
        dir.write("subdir/data.yaml", "data: 2\n");
        dir.write("subdir/data.txt", "ignored\n");

        let project = load_project(&dir.path().join("main.yml")).expect("loads cleanly");

        assert!(project.namespaces.get("", "main").is_some());
        assert!(
            project.namespaces.get("", "data").is_some(),
            "data from subdir"
        );
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

    // ---- StdExecutor tests ----

    #[test]
    fn std_executor_sh_echoes_output() {
        let exec = StdExecutor;
        let result = exec.execute("sh", "echo hi").expect("sh executes");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "hi\n");
    }

    #[test]
    fn std_executor_unknown_backend_is_err() {
        let exec = StdExecutor;
        let err = exec.execute("ruby", "puts 1").expect_err("unknown backend");
        match err {
            ExecError::UnknownBackend(name) => assert_eq!(name, "ruby"),
            other => panic!("expected UnknownBackend, got {other:?}"),
        }
    }

    #[test]
    fn std_executor_nonzero_exit_code() {
        let exec = StdExecutor;
        let result = exec.execute("sh", "exit 1").expect("spawns ok");
        assert_eq!(result.exit_code, 1);
    }

    // ---- meta value tests ----

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

    // ---- _c override tests ----

    #[test]
    fn load_project_with_override_merges_components() {
        let dir = TempDir::new();
        dir.write("main.yml", "a: 1\nb: 2\n");

        let project = load_project_with_override(&dir.path().join("main.yml"), Some("b: 99\nc: 3"))
            .expect("loads cleanly");

        let a_def = project.namespaces.get("", "a").expect("a exists");
        assert_eq!(node_to_value(&a_def.body), value_of("1\n"));

        let b_def = project
            .namespaces
            .get("", "b")
            .expect("b exists (overridden)");
        assert_eq!(node_to_value(&b_def.body), value_of("99\n"));

        let c_def = project.namespaces.get("", "c").expect("c exists (added)");
        assert_eq!(node_to_value(&c_def.body), value_of("3\n"));
    }

    #[test]
    fn load_project_with_override_none_is_identity() {
        let dir = TempDir::new();
        dir.write("main.yml", "a: 1\n");

        let project =
            load_project_with_override(&dir.path().join("main.yml"), None).expect("loads cleanly");

        let a_def = project.namespaces.get("", "a").expect("a exists");
        assert_eq!(node_to_value(&a_def.body), value_of("1\n"));
    }

    #[test]
    fn load_project_with_override_discards_ymx_from_override() {
        let dir = TempDir::new();
        dir.write("main.yml", "_ymx:\n  plain: false\nmain: 1\n");

        let override_yaml = "_ymx:\n  plain: true\nmain: 2";
        let project = load_project_with_override(&dir.path().join("main.yml"), Some(override_yaml))
            .expect("loads cleanly");

        let main_def = project.namespaces.get("", "main").expect("main exists");
        assert_eq!(
            node_to_value(&main_def.body),
            value_of("2\n"),
            "main overridden to 2"
        );

        assert_eq!(
            project.raw_meta_ymx.len(),
            1,
            "only the file's _ymx is stored (override's discarded)"
        );
        assert_eq!(
            project.raw_meta_ymx[0].1,
            value_of("plain: false\n"),
            "file's _ymx preserved"
        );
    }

    #[test]
    fn load_project_with_override_discards_test_from_override() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n_test:\n  main: 1\n");

        let override_yaml = "main: 2\n_test:\n  main: 2";
        let project = load_project_with_override(&dir.path().join("main.yml"), Some(override_yaml))
            .expect("loads cleanly");

        let main_def = project.namespaces.get("", "main").expect("main exists");
        assert_eq!(
            node_to_value(&main_def.body),
            value_of("2\n"),
            "main overridden to 2"
        );

        assert_eq!(
            project.raw_meta_test.len(),
            1,
            "only the file's _test is stored (override's discarded)"
        );
        assert_eq!(
            project.raw_meta_test[0].1,
            value_of("main: 1\n"),
            "file's _test preserved"
        );
    }

    #[test]
    fn load_project_with_override_discards_file_scoped_from_override() {
        let dir = TempDir::new();
        dir.write("main.yml", "_helper: 2\nmain: 1\n");

        let override_yaml = "_helper: 4\nmain: 3";
        let project = load_project_with_override(&dir.path().join("main.yml"), Some(override_yaml))
            .expect("loads cleanly");

        let main_def = project.namespaces.get("", "main").expect("main exists");
        assert_eq!(
            node_to_value(&main_def.body),
            value_of("3\n"),
            "main overridden to 3"
        );

        let main_id = file_id_of(&project, "main.yml");
        let helper_def = project
            .file_scoped
            .get(main_id, "_helper")
            .expect("_helper exists (from file)");
        assert_eq!(
            node_to_value(&helper_def.body),
            value_of("2\n"),
            "file-scoped _helper keeps file's value, not override's"
        );

        let override_id = FileId(1);
        let override_helper = project.file_scoped.get(override_id, "_helper");
        assert!(
            override_helper.is_none(),
            "override's _helper is not registered in any file_scoped store"
        );
    }

    #[test]
    fn load_project_with_override_invalid_yaml_returns_error() {
        let dir = TempDir::new();
        dir.write("main.yml", "main: 1\n");

        let err =
            load_project_with_override(&dir.path().join("main.yml"), Some(":\n  :\n  invalid"))
                .expect_err("invalid override YAML should error");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, E001);
        assert_eq!(
            err[0].file.as_deref(),
            Some(dir.path().join("main.yml/.ymx-override").as_path()),
            "error carries synthetic override path (root.join)"
        );
    }

    #[test]
    fn load_project_with_override_empty_override_is_identity() {
        let dir = TempDir::new();
        dir.write("main.yml", "a: 1\n");

        let project = load_project_with_override(&dir.path().join("main.yml"), Some(""))
            .expect("loads cleanly");

        let a_def = project.namespaces.get("", "a").expect("a exists");
        assert_eq!(node_to_value(&a_def.body), value_of("1\n"));

        assert!(
            project.namespaces.get("", "b").is_none(),
            "no extra defs added from empty override"
        );
    }
}
