## Library API

The public surface is spread across three crates so each concern stays small and optional. `ymx-lib` is a thin façade that re-exports `ymx-core`'s compiler types and adds only a directory-walking `load_project` helper; it deliberately contains **no** `_ymx`/`_test` logic.

### `ymx-core` — compiler types (re-exported by `ymx-lib`)

```rust
// Core IR and diagnostics (definitions live in ymx-core; ymx-lib re-exports them).
#[serde(untagged)]
pub enum Value { Null, Bool(bool), Int(i64), Float(f64), String(String), Array(Vec<Value>), Object(IndexMap<String, Value>) }

pub struct FileId(pub u32);   // index into `Project::files`
pub struct Span { pub line: u32, pub col: u32 }

/// `file` is the resolved host-file path (None only when no file context exists).
/// It is resolved at creation so load-time diagnostics (which have no `Project`
/// to resolve against under the all-or-nothing `load_project`) still render.
pub struct Diagnostic {
    pub file: Option<PathBuf>,
    pub line: u32,
    pub col: u32,
    pub component: Option<String>,
    pub code: &'static str,
    pub message: String,
}

pub enum Format { Json, Diagnostics }

/// Namespace-promotion mode for `_ymx.plain` / `--plain` / `--plain-template`.
pub enum PlainMode { False, All, TemplatesOnly }

pub struct Options {            // consumed by `compile`
    pub entry: String,          // default "main.main" — a file-path entry path, not a bare name
    pub from_keyword: String,   // default "from"

    pub max_depth: u32,         // default 256
    pub pretty: bool,           // default false
    pub format: Format,         // default Json
    pub plain: PlainMode,       // default False
    pub allowed_backends: Option<Vec<String>>,  // None = all backends allowed
    pub executor: Option<Arc<dyn CommandExecutor>>,  // None = shell execution disabled (E016)
    pub allowed_ipc: Option<Vec<String>>,         // None = all transports allowed; IPC components (rule 21) use the separate `IpcHost` trait below.
    pub ipc: Option<Arc<dyn IpcHost>>,           // None = IPC calls fail with E018
}

/// A loaded project: the merged component namespace (global + sub-namespaces),
/// file-scoped components, and the raw parsed values of the reserved meta keys
/// (`_ymx`, `_test`) per document — uninterpreted by ymx-core.
pub struct Project {
    pub files: Vec<PathBuf>,   // files[FileId.0] — host-file path of every loaded document
    /* namespaces, file_scoped, raw_meta_ymx: Vec<(FileId, Value)>, raw_meta_test: Vec<(FileId, Value)> */
}

/// Call arguments (named and/or positional) for `compile_component`.
pub enum Args { None, Named(Vec<(String, Value)>), Positional(Vec<Value>), Mixed { named: Vec<(String, Value)>, positional: Vec<Value> } }

/// Lower-level pure entry point: resolve `component` (a bare name resolved in
/// the global namespace, or a dotted namespace path `subdir.comp`) called with
/// `args`, under `opts`. This is what `ymx-test::run_tests` uses to exercise
/// test targets (`parse_tests` has already enforced the same-document rule and
/// emits the dotted form for sub-namespace targets).
pub fn compile_component(project: &Project, component: &str, args: &Args, opts: &Options) -> Result<Value, Vec<Diagnostic>>;

/// Convenience: resolve the entry path `opts.entry` (file-path form
/// `<folder.path>.<file>.<comp>`) — the middle file segment selects the
/// front-matter source, and `<folder>.<comp>` is the namespace-qualified
/// component compiled with no args.
pub fn compile(project: &Project, opts: &Options) -> Result<Value, Vec<Diagnostic>>;

/// Output of a successful shell command execution.
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Error from a failed shell command execution.
pub enum ExecError {
    UnknownBackend(String),
    SpawnFailed(String),
}

/// Executor for shell commands (backing the `sh`/`pw` builtin components — rule 19).
/// Implemented by the caller (ymx-lib provides StdExecutor); ymx-core stays I/O-free.
pub trait CommandExecutor: Send + Sync {
    fn execute(&self, backend: &str, command: &str) -> Result<ExecOutput, ExecError>;
}

/// IPC components (rule 21) use the separate `IpcHost` trait below.
pub trait IpcHost: Send + Sync {
    fn call(&self, name: &str, spec: &IpcSpec, request: IpcRequest) -> Result<IpcResponse, IpcError>;
    fn shutdown(&self);
}

pub struct IpcRequest { pub args: Args }

pub struct IpcResponse {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<u16>,
}

pub enum IpcError {
    NoHost,
    DisallowedTransport(String),
    SpawnFailed(String),
    Crashed,
    Timeout,
    FramingError(String),
    StatusCode(u16, String),
    HookFailed(String),
    Custom(String),
}

#[derive(Debug, Clone)]
pub struct IpcSpec {
    pub runner: IpcRunner,
    pub transport: IpcTransport,
    pub protocol: IpcProtocol,
    pub request_template: String,
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
    pub startup_timeout: u32,
    pub ready: Option<String>,
    pub request_timeout: u32,
    pub stop_signal: String,
    pub stop_message: Option<String>,
    pub stop_timeout: u32,
    pub before_start: Option<String>,
    pub after_start: Option<String>,
    pub before_stop: Option<String>,
    pub after_stop: Option<String>,
    pub prelude: Option<String>,
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<indexmap::IndexMap<String, String>>,
    pub query: Vec<String>,
    pub body: IpcHttpBody,
    pub ok_status: Vec<u16>,
    pub addr: Option<String>,
    pub path: Option<String>,
    pub env: Option<indexmap::IndexMap<String, String>>,
    pub cwd: Option<String>,
    pub restart: IpcRestart,
    pub max_restarts: u32,
    pub lazy: bool,
}

pub enum IpcRunner { Process, External }
pub enum IpcTransport { Pipe, Socket, Http }
pub enum IpcProtocol { Line, Sentinel, Raw, Json, JsonRpc }
pub enum IpcMode { Text, Json }
pub enum IpcParse { None, Yaml, Json }
pub enum IpcEnvelope { Payload, Full }
pub enum IpcStderr { Ignore, Capture, Fail }
pub enum IpcHttpBody { All, Arg0, Arg(String), Off }
pub enum IpcRestart { Never, OnFailure }
```

### `ymx-lib` — thin façade

```rust
/// Walks `root` (`.yml`/`.yaml`), parses each document with spans, builds the
/// `Project` (namespace merge, duplicate/file-scope/reserved-name checks), and
/// collects raw `_ymx`/`_test` meta values without interpreting them.
/// I/O lives here so ymx-core stays I/O-free.
pub fn load_project(root: &Path) -> Result<Project, Vec<Diagnostic>>;

// Re-exports from ymx-core: Value, Diagnostic, Options, Format, Project, compile.
```

`ymx-lib` does **not** depend on `ymx-config` or `ymx-test`; library users who want front-matter-driven options or inline tests depend on those crates directly and compose the pipeline (see `ymx-cli` for the canonical orchestration).

### `ymx-config` — front-matter → Options

```rust
/// Per-flag CLI override (None = not provided on the command line).
pub struct CliOverrides {
    pub entry: Option<String>,
    pub from_keyword: Option<String>,

    pub max_depth: Option<u32>,
    pub pretty: Option<bool>,
    pub format: Option<Format>,
    pub plain: Option<PlainMode>,
    pub allowed_backends: Option<Vec<String>>,
}

/// Applies precedence CLI > entry-file `_ymx` > engine default and returns the
/// effective `Options`. The entry file is the document located by the entry
/// path (CLI `--entry` if set, else `main.main`); the file stem is the
/// penultimate path segment and the component is the last segment.
pub fn extract_options(project: &Project, cli: &CliOverrides) -> Result<Options, Vec<Diagnostic>>;
```

### `ymx-test` — inline tests

```rust
pub enum TestArgs { None, Named(Vec<(String, Value)>), Positional(Vec<Value>), Scalar(Value) }

/// What a test asserts about its target.
pub enum Expected {
    /// The target must compile to this value.
    Value(Value),
    /// The target must produce a diagnostic with the given code.
    Error { code: String },
}

pub struct Test { pub target: String, pub args: TestArgs, pub expected: Expected, pub file: FileId, pub span: Span }

pub struct TestResult { pub test: Test, pub actual: Result<Value, Vec<Diagnostic>>, pub passed: bool }

/// Parses `_test` meta blocks into concrete tests (same-file targeting).
pub fn parse_tests(project: &Project) -> Result<Vec<Test>, Vec<Diagnostic>>;

/// Runs each test by compiling its target component with `args` under `opts`
/// against an already-loaded `Project` (the caller runs `load_project` first; a
/// failed load yields no `Project` and thus no tests run — see [Reach of the
/// error variant](06-project-metadata.md#_test--inline-tests)).
/// For `Expected::Value`, `passed` is true iff `actual` is `Ok(v)` with `v == expected`.
/// For `Expected::Error`, `passed` is true iff some diagnostic observed across the
/// harness's pipeline (`extract_options` → `compile_component`) for this test's
/// target has `code == expected.code`. Only codes arising after a successful
/// load are matchable (load-time codes are not `_test`-driveable).
pub fn run_tests(project: &Project, opts: &Options) -> Vec<TestResult>;
```

### `ymx-cli` — argument parsing

```rust
/// Parsed command-line arguments (produced by `ymx_cli::parse()`).
pub struct ParsedCli {
    pub path: PathBuf,           // resolved input path (file or directory)
    pub entry: Option<String>,  // --entry override (None = use file stem as comp)
    pub test_dir: Option<PathBuf>, // set when `--test` is given and `path` resolves
                                   // to a directory at parse time (recursive mode);
                                   // None means single-project file mode
    /* other fields: from_keyword, max_depth, output, pretty,
       plain, plain_template, format */
}
```

### Serialization & errors

- `Value` serializes to JSON with insertion-ordered object keys (`serde_json` + `preserve_order`). On success with `format = Json`, callers serialize `Value` (pretty or compact per `pretty`); with `format = Diagnostics`, there are no diagnostics to emit on success.
- `Err(Vec<Diagnostic>)` from `load_project`/`compile`/`extract_options`/`parse_tests` carries all errors collected during namespace resolution, front-matter validation, or compilation; the CLI renders them per the *Diagnostics* format.
