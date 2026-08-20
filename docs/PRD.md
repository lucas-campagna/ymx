# YMX

A YAML parser and compiler for documents, HTML, and PDF — usable as a CLI tool, a WEB service, and a library.

## Purpose of the project

YAML is human-friendly to read and write. YMX uses that property to let authors describe rich, reusable, composable documents that compile to HTML, PDF, or arbitrary JSON-like structures.

The project provides a tool/compiler that turns YAML source files into documents, PDFs, and HTML, while keeping the authoring experience simple and declarative.

## Terminology

- **Document**: a single YAML source file parsed by YMX.
- **Component**: each top-level key-value pair in a document defines a component. The key is the component's name and the value gives its content (rule 1).
- **Property**: a key-value pair inside a component. Properties are also the arguments the component accepts when called.
- **Argument**: a value passed to a component when it is called. Arguments are referenced in component bodies as `$name` (named) or `$0`, `$1`, `$2`, … (positional).
- **Template component**: a component whose name starts with `$` (e.g. `$box`). Templates are applied automatically after the component that uses them is called (rule 5). Templates can chain indefinitely (`$a`, `$$a`, `$$$a`, …).
- **Entry**: the top-level component chosen for compilation. On the CLI, the mandatory positional argument is a **file** path; `--entry` (default `main`) selects the component within that file. The CLI derives the entry path internally as `<file_stem>.<component>` (always exactly 2 segments: empty folder.path + file stem + component). The entry path is a file-path address, distinct from the namespace dotted path used by `from` / `$name` resolution (see *Multi-file projects*). Defaults to `main.main`; overridable with `--entry`. In library code, `Options.entry` is set directly to the full entry path.
- **Namespace**: the scope a component lives in. The project root is the global namespace; each subdirectory is a sub-namespace addressed by a dotted path (e.g. `subdir.comp`).
- **Meta key**: a reserved top-level key (`_ymx` or `_test`) that is not a component but carries project metadata — front-matter flag defaults or inline tests (see *Project metadata*).
- **Front matter**: the `_ymx` meta block of a document, supplying compiler-flag defaults.

## Technologies

The project is written in Rust. Rust provides type and memory safety without a garbage collector, which suits a long-lived, performance-sensitive tool.

**Toolchain.** The workspace targets Rust **edition 2021**; the MSRV is the latest stable release at the time of development (pinned in `rust-toolchain.toml`). JSON serialization keeps object-key insertion order via `serde_json` with the `preserve_order` feature (backed by `indexmap`). YAML is parsed with `yaml-rust2`, preserving line/column spans on every scalar for diagnostics.

## Scope

YMX is being built in versions. The rules in this document describe the language and are stable across versions; the *output targets* arrive incrementally.

**v1 (current)**: the resolver for rules 1–18, emits JSON. CLI and library only. HTML, PDF, and WEB are intentionally not in v1.

**v2**: HTML renderer + CLI flag to pick the target.

**v3**: PDF renderer (backend choice deferred until needed).

**Future**: WEB service; swappable math/engine backends (Lua, Python, JavaScript); user-defined builtins via a plugin system.

## Architecture

The project is a Cargo workspace of multiple crates:

```
ymx/
├── Cargo.toml
├── crates/
│   ├── ymx-core/    # parser, resolver, math, builtins, diagnostics (no I/O)
│   ├── ymx-config/  # _ymx front-matter extraction -> Options (no I/O)
│   ├── ymx-test/    # _test parsing + run_tests / TestResult (no I/O)
│   ├── ymx-lib/     # thin stable API: re-exports ymx-core + load_project I/O helper
│   ├── ymx-cli/     # binary: arg parsing, orchestration (load→extract→compile→emit), --test
│   └── ymx-web/     # stub crate (v3+)
└── tests/
    └── cases/
        └── rule-NN/<scenario>/   # one directory per scenario (see Testing)
```

- **Crate boundaries**: `ymx-core` is the pure compiler — parsing, the rule-1–16 resolver, the math engine, and builtins, with no filesystem or network I/O. `ymx-config` owns the `_ymx` front-matter logic (parsing the meta key, applying the *CLI > entry-file* precedence, producing `Options`). `ymx-test` owns the `_test` meta-key logic (parsing tests, running them, `TestResult`). `ymx-lib` is intentionally small: it re-exports `ymx-core`'s public surface and adds only a thin `load_project` I/O helper that walks a directory and builds a `Project` (collecting raw `_ymx`/`_test` meta values without interpreting them). The `_ymx`/`_test` *logic* lives in `ymx-config`/`ymx-test`, never in `ymx-lib`. `ymx-cli` depends on `ymx-lib` (for `load_project` + core types), `ymx-config`, and `ymx-test`, and orchestrates `load_project` → `extract_options` → `compile` → emit (and `run_tests` under a `--test` flag).
- **YAML parsing**: `yaml-rust2` is used directly (inside `ymx-core`) so source spans (line/column) are preserved on every scalar for diagnostics.
- **Output**: YAML → intermediate `Value` IR → serialize to JSON (v1). The IR is `Null | Bool | String | Int(i64) | Float(f64) | Array | Object`; object keys preserve YAML insertion order. HTML/PDF renderers consume the same IR in later versions.
- **Math**: a `MathEngine` trait evaluates `${...}`. v1 uses dynamic operand resolution — a String-valued operand is re-scanned as a math expression when it parses as one (see rule 7), numerically coerced when possible, and `+` falls back to string concatenation otherwise. The trait is the boundary for swapping to a Lua/Python/JavaScript engine in the future.
- **Builtins**: a `Builtin` trait. v1 ships `$map`, `$reduce`, `$merge`. Each builtin is a *special form* that declares its own argument-evaluation strategy (e.g. `$map`/`$reduce` keep their first argument unevaluated as a callable component). The trait is the future plugin boundary.
- **Diagnostics**: structured `Diagnostic { file, line, col, component, code, message }` (where `file` is the resolved file path) rendered to stderr as `[code] file:line:col (component): message` and surfaced identically through `--format diagnostics` (one diagnostic per line, no JSON on stdout). Designed so a richer "bug report" mode (full call-stack + local-argument dump) can be added later without breaking the API. The stable error codes are listed in *Diagnostic codes* below.
- **Cycles**: no precise cycle detection in v1; a configurable depth cap (`--max-depth`, default 256) prevents runaway recursion and surfaces as a "max-depth exceeded" diagnostic (`E008`). On entry to each recursive operation in rule 11's pipeline — each inline `$comp(...)` call (rule 3), each math `comp(...)` call (rule 7), each template step in a chain (rule 5), and each `from` dispatch (rule 6) — the counter is **checked before** incrementing: if `depth == max_depth` the operation aborts with `E008` (so at most `max_depth` recursive operations are allowed); otherwise `depth` is incremented by 1 and the operation proceeds.

### Diagnostic codes

Every diagnostic renders to stderr as `[code] file:line:col (component): message`. Load-time codes (`E001`, `E004`, `E007`, `E015`) are not `_test`-driveable because `load_project` is all-or-nothing; they are exercised by crate `#[test]` unit tests with inline YAML.

| Code | Stage | Diagnostic | Example |
|------|-------|------------|---------|
| `E001` | load | YAML parse error or unsupported YAML feature (multi-document stream `---`, complex mapping key, merge key `<<`); also the load-stage catch-all for I/O failures (missing root, unreadable file) and non-string top-level keys. | `a: 1\n---\nb: 2` in one file → multi-document stream not supported. `0: value` → non-string top-level key. |
| `E002` | compile | Unknown component reference. | `a: $nonexistent` → `a` references a component that does not exist. |
| `E003` | compile | Missing required argument. A property `$x` is referenced but neither supplied by the caller nor declared with `x?:`. | `a: $x` called with no `x` argument → missing required `x`. |
| `E004` | load | Duplicate component name in the same namespace. Two top-level definitions of `foo` in the same directory. | `a.yml` defines `x: 1` and `b.yml` also defines `x: 2` → both in the global namespace → `E004`. |
| `E005` | compile | File-scope violation. A `_`-prefixed component is referenced from outside its document. | `_helper: 42` in `a.yml`; `b.yml` calls `$a(_helper=$_helper)` → cross-document reference to file-scoped `_helper` → `E005`. |
| `E006` | compile | Ambiguous shortcut. Two properties in the same component body both match names of existing components. | `a:\n  b: 1\n  c: 2\nb: $default\nc: $default` — both `b` and `c` match components → shortcut is ambiguous → `E006`. |
| `E007` | load | Reserved name used as a component or template (`map`/`reduce`/`merge`). | `map: 1` → defining a component named `map` (or `$map`, `$$map`, …) is rejected. |
| `E008` | compile | Max-depth exceeded. The recursion depth cap (`--max-depth`, default 256) was exhausted. | A chain of 300 nested `$a()` calls hits the 256-deep limit → `E008`. |
| `E009` | options | Entry not found. Entry path has fewer than two segments, resolves to a missing file, has an ambiguous `.yml`/`.yaml` stem, or the entry name is not a valid component identifier. | `ymx .` with no `main.yml`; `ymx ./folder` where both `folder.yml` and `folder.yaml` exist; `ymx ./file.yml --entry NOT_A_VALID_NAME`. |
| `E010` | both | Invalid syntax: malformed call-site, bad string escape (`\X`), math identifier prefix `$letter`, mixed-shape template chain, unknown/invalid `_ymx` field, or malformed `_test` block. Also the v1 rejection of rule-18 constructs (`$` suffix on property key, `?$` combination). | `\n` in a string (unknown escape); `${ $x }` (bare `$` in math — drop the `$`); `plain: "maybe"` (invalid enum value); `x$?: y` (wrong modifier order). |
| `E011` | compile | Math error (type mismatch, division by zero, non-numeric operand) or builtin argument type error (non-array 2nd arg to `$map`/`$reduce`; mixed-shape `$merge`). | `${true + 1}` → Bool + Int is a type mismatch. `$map(a, "not an array")` → second arg must be Array. `$merge({a:1}, [1,2])` → Object + Array is an unsupported merge shape. |
| `E012` | compile | Positional argument after a named argument in a call. | `$f(name=1, 2)` → positional arg `2` follows named `name=1` → `E012`. |
| `E013` | compile | Array/object literal as a direct call argument. Not supported in v1. | `$f([1,2,3])` or `$g({a:1})` → v1 does not support passing inline collection literals as call arguments. |
| `E015` | load | Meta-key reserved name used as a component or template. A top-level key that is a leading-`$` variant of `_ymx` or `_test`. | `$_ymx:\n  v: 1` or `$$_test: 2` → leading-`$` variants of meta keys are rejected as reserved. |
| `E016` | compile | Shell execution error (unknown backend, disallowed backend, spawn failure, or executor not provided). | `$sh{echo hi}` with no executor → E016. `$pw{...}` when `allowed_backends: [sh]` → E016. |

## Multi-file projects

A project consists of a single **entry file** plus any files it references via the `_use` directive. Loading is explicit and import-based — no automatic recursive directory walk by default.

**`_use` — explicit file imports.** `_use` is a meta key (like `_ymx`, `_test`). It lives in the entry file's document and/or in any file loaded via `_use`. It comes in three forms:

- **`*`** (bare string `_use: *`): recursive wildcard — walk all `.yml`/`.yaml` files under the entry file's directory, skipping `.git`, hidden directories, and subdirectories containing no YAML files directly. This is the default when no `_use` is present.
- **Wildcard object** (`_use: {"*": "foo"}`): import all public (non-`_`-prefixed) components and templates from `foo.yml` (or `foo.yaml`) into the global namespace, keeping their original names. The file stem `foo` resolves with the same E009 rules as the entry path.
- **Named imports** (`_use: {x: "foo.bar"}`): import component `bar` from `foo.yml` and make it available as `x` in the global namespace. The RHS is `<file>.<component>` — the file stem must resolve to exactly one `.yml`/`.yaml` file (E009 on ambiguity or missing); the component must exist in that file (E002). Multiple files may define the same component name internally; renaming on import prevents namespace collisions.

All imported components land in the **entry's global namespace**. File-scoped components (`_`-prefixed) are never importable (E005). Components may be renamed on import, so two imported files may both define `main` internally without conflict — as long as the local aliases differ.

**Transitive `_use`**: an imported file's own `_use` is also processed. If `main.yml` imports `utils.yml` and `utils.yml` imports `math.yml`, all components from `math.yml` are available in `main`'s global namespace too. Cycles are detected and raise `E001`.

**Path syntax**: the dotted path in `_use` values (`foo.bar`) follows the same `<file>.<component>` notation as `from:` — the file stem is the first segment, the component is the last. Only the global namespace is reachable via `_use`; sub-namespace prefixes are not allowed.

> **Backward compatibility.** When no `_use` key is present in the entry file, the engine behaves as if `_use: *` were given — a recursive walk of the entry file's directory, with the same filtering rules as the previous automatic behaviour. This means existing single-file projects continue to work unchanged.

## Project metadata

A document may carry three reserved **meta keys** at its top level — `_ymx` (front matter), `_test` (tests), and `_use` (file imports). They are not components (see *Reserved names*); `ymx-core` strips them from the namespace and stores their raw parsed values on the `Project`, and the `ymx-config` / `ymx-test` / `ymx-lib` crates interpret them. A document with two top-level `_ymx` (or `_test` or `_use`) keys keeps the first and ignores the rest; meta keys are consumed, not registered, so E004 (duplicate name) does not apply.

### `_ymx` — front matter

`_ymx` is a mapping of compiler-flag **defaults** for the document. The recognized fields, all optional:

| Field             | Type        | Default     | Notes |
|-------------------|-------------|-------------|-------|
| `max_depth`       | int         | `256`       | recursion cap (rule 11); exceeding it raises `E008` |
| `from_keyword`    | string      | `from`      | rule 6 keyword |
| `default_keyword` | string      | `default`   | rule 8 keyword; the engine prefixes `$` internally |
| `format`          | string      | `json`      | `json` or `diagnostics` |
| `pretty`          | bool        | `false`     | pretty-print JSON output |
| `plain`           | string enum | `false`     | `false` \| `true` \| `template`; promotes sub-namespace names into the global namespace (`true` = components **and** templates; `template` = templates only). Invalid value → `E010`. See *Component visibility* |
| `allowed_backends` | list of strings | (all) | Restricts which executor backends may be used (rule 19). If absent, all backends are allowed. If set (e.g. `[sh]`), only those backends are permitted — using a non-listed backend emits E016. |

> `entry` is intentionally **not** a `_ymx` field: the entry determines which document's `_ymx` block is the project's front-matter source, so it is resolved before front matter is read. The entry is therefore CLI-only: `--entry <component>` if provided, else `main`. The CLI receives a file path as the positional argument; the project root is the file's parent directory, and the entry path is derived as `<file_stem>.<component>` (always exactly 2 segments). The entry path is a **file-path address** (`<folder.path>.<file>.<component>`), distinct from the namespace dotted path used by `from` / `$name` resolution: `main.main` → root folder + `main.yml` + component `main`; `b.x` → file `b.yml` in the project root + component `x`. One segment is not a valid entry path (`E009`). The entry *file* must exist (else `E009`). The entry *component* is **not** required to exist at load/option-resolution time — it is only required when something actually compiles it (CLI compile mode, or a bare-A/B `_test` targeting the entry); a missing entry component at that point surfaces as `E002` (unknown component reference), like any other unknown component ref. This lets `_test`-only documents that target other components via type-2 maps omit the entry component entirely.

**Precedence.** For each flag, the effective value is **CLI flag (if provided) > entry-file front matter > engine default**. The *entry file* — the document whose `_ymx` block supplies front matter — is the document located by the entry path (CLI `--entry` if set, else `main.main`): the resolved file must exist, else `E009`. `_ymx` blocks in other documents are **completely ignored** — never parsed or validated (an unknown field there is not an error; the block may even be malformed). An unknown `_ymx` field, or an invalid value for a known field (e.g. `plain: "maybe"`), in the **entry** file's `_ymx` is a hard error (`E010`).

### `_test` — inline tests

`_test` is a sibling of `_ymx` (also a top-level meta key, not nested under `_ymx`). It describes expected outcomes for components **defined in the same document**. A test value has two forms, and form B has two variants:

- **A** — a literal expected value: the target component called with **no arguments** must compile to a value equal to A.
- **B** — a mapping. B has two variants:
  - **Value variant** — `{args: <args>, result: <expected>}`: the target component called with `args` must compile to a value equal to `result`.
  - **Error variant** — `{args: <args>, error: <code>}`: the target component called with `args` must produce a diagnostic whose `code` equals `<code>` (e.g. `"E002"`). `error` and `result` are mutually exclusive; a B mapping containing neither, both, or a non-string `error` value is a malformed `_test` block (`E010`).

  In both variants `args` is optional (absent = no arguments). The `args` shape mirrors the call-site grammar (rule 3): a mapping (named arguments), a list (positional, binding `$0`, `$1`, …), or a scalar (binds `$0`).

  > `args` values are taken **literally** as YAML values — they are **not** interpolated at `_test`-parse time. Any `$name`, `${...}`, or `$call(...)` appearing inside an `args` value is resolved by the **target** component against the arguments the test binds it, never by the test harness against an (empty) test scope. To exercise interpolation, bind the raw input via `args` and assert the interpolated output as `result`.

`_test` at the top level may be one of:

1. **Bare A** — a scalar targeting the entry component (no args). (Top-level mappings and lists are never bare A — see disambiguation below.)
2. **Bare B** — a mapping containing the key `result` or the key `error`, targeting the entry component.
3. **Type-2 map** — a mapping `{<compname>: A_or_B, ...}` where each key names a component defined in the **same document**; each value is an A or a B for that component.
4. **List of type-2 maps** — a list whose elements are type-2 maps (a top-level list is always shape 4, never a bare-A list).

**Disambiguation.** A top-level mapping is interpreted as bare B (shape 2) if it contains a `result` or `error` key, otherwise as a type-2 map (shape 3); a top-level list is always shape 4. Consequently a bare A whose expected value is a mapping or a list cannot be written bare — test the entry with such an expectation via a type-2 map keyed by the entry name (e.g. `{main: {…}}` or `{main: [...]}`) or via bare B (`{result: {…}}` / `{result: [...]}`). A scalar bare A targets the entry directly. Inside a list (shape 4) every element is a type-2 map (never bare A/B), so wrapping a type-2 map in a list forces the type-2 reading even when a target happens to be named `result`, `args`, or `error`.

> Form A and B (no `args`) coincide: `expected: V` equals `{result: V}`. B exists to supply `args` and/or an error expectation. A target whose name is `result`, `args`, or `error` is discouraged in test files; the list-wrapping escape above disambiguates if needed.

**Scope.** Every component named in a type-2 map must be defined in the same document as the `_test` block; referencing a component from another document (or a namespaced one) is `E002`. The entry targeted by bare A/B is the project entry (`--entry` path if set, else `main.main`), resolved by entry-path lookup (not restricted to the `_test`-hosting document).

**Reach of the error variant.** `load_project` is **all-or-nothing**: any load-time diagnostic aborts with `Err` and no `Project` is produced, so `run_tests` never runs for a project that fails to load. The error variant therefore asserts codes that arise **after** a successful load — option-resolution (`E009`, the unknown/invalid-`_ymx`-field part of `E010`) and target-compilation (`E002`, `E003`, `E005`, `E006`, `E008`, the call-site / string-escape / math-identifier / mixed-shape-chain parts of `E010`, `E011`, `E012`, `E013`). Load-time codes (`E001`, `E004`, `E007`, `E015`) are **not** `_test`-driveable (the project that would surface them never loads). Matching is by code only: a test passes iff some diagnostic observed across the harness's pipeline (`extract_options` → `compile_component` of the test's target, run against an already-loaded `Project`) has `code` equal to the asserted code.

> Diagnostics that are unreachable by construction — the malformed-`_test`-block case of `E010`, and YAML-parse failures (`E001`) of the document that hosts the `_test` block (an un-parseable carrier file is unreadable), plus all other load-time codes (`E004`, `E007`, `E015`) — are exercised by ordinary crate `#[test]` unit tests with inline YAML snippets (see *Testing*). Every other code is reachable: option-resolution and target-compilation errors fire after `_test` is parsed, against the loaded `Project`.

### `_use` — file imports

`_use` is a mapping of explicit file imports. Its value has three forms:

- **Bare `*`**: `_use: *` — recursive wildcard. The entry file's directory is walked for all `.yml`/`.yaml` files (same filter as the legacy behaviour: skip `.git`, skip hidden directories, skip subdirectories with no YAML files directly). All public components/templates found are added to the entry's global namespace.
- **Wildcard string**: `_use: {"*": "foo"}` — import all public components and templates from `foo.yml` into the global namespace, keeping their original names.
- **Named entries**: `_use: {x: "foo.bar", ...}` — for each `alias: "file.component"`, import `component` from `file.yml` and register it under `alias` in the global namespace. The file must exist and the component must be defined (E002 if not). Multiple files may define the same component internally; renaming prevents collisions.

All imported components land in the **global namespace** of the entry file. File-scoped components (`_`-prefixed) cannot be imported (E005). If an imported file has its own `_use`, that is processed too (transitive imports). A cycle in the import graph raises `E001`.

Only the **entry file's** `_use` and those of its transitively imported files are processed. `_use` in other files (not imported) is ignored.

## CLI

```
ymx [path] [flags]
```

**Stdin modes.** `ymx` uses stdin in two implicit ways, distinguished by whether `path` is present:

- **No `path` — stdin is the script.** `cat script.yml | ymx` is equivalent to `ymx script.yml`. The full stdin content is treated as a YAML document; it is written to a temporary file (`main.yml`) inside a temporary project directory. The entry is `main.main` (or `--entry` overrides). If stdin is a terminal (tty) and no `path` is given, the CLI exits 2 with a usage error.

  > Escape expansion (`\n`, `\t`, `\\`) is applied to stdin content before YAML parsing, same as `-c`.

- **`path` given — stdin is call arguments.** `echo '{"a":123}' | ymx script.yml` calls the `main` component of `script.yml` with `a=123`. The positional argument `path` is the entry file (derived as `<file_stem>.<component>`, default `main.main`); the project root is the file's parent directory. Stdin is read as call arguments: it is first parsed as JSON; if that fails it is retried as YAML. The resulting value is converted to `Args`:
  - `Value::Object` → `Args::Named([(k,v), …])` (sorted by key for determinism)
  - `Value::Array` → `Args::Positional([v0, v1, …])`
  - `Value::Scalar` (number / string / bool / null) → `Args::Positional([value])` — binds to `$0`
  The CLI calls `compile_component(project, "<entry>", &args, &opts)` instead of `compile`. If stdin is a terminal (tty) and `path` is given, the CLI exits 2 with a usage error.

`-`**`--test` is unaffected by stdin modes**; it requires a `path` argument and does not read stdin in either case.

**Inline code (`-c`).** The `-c` flag provides inline component definitions as a YAML or JSON string. When combined with a file, `-c` components override matching names from the file (complete replacement, not property-level merge). When used alone, `-c` is the entire script. Stdin can still provide call arguments when a file is present.

> **Escape expansion.** The `-c` value undergoes escape expansion before YAML parsing: `\n` becomes a newline, `\t` becomes a tab, and `\\` becomes a literal backslash. Any other `\X` is passed through unchanged (the YMX string parser will catch truly invalid escapes). This lets users write multi-line YAML in a single-quoted shell string, e.g. `ymx -c 'main: $comp()\ncomp: a * b'`.

Examples:
```bash
ymx -c 'main: hello world'                           # → "hello world"
ymx -c '{"main": "${1 + 1}"}'                        # → 2
echo '{"a": 1, "b": 2}' | ymx -c "main$: a + b"     # → 3 (stdin = args)
echo -e 'a: 10\nb: 22' | ymx -c '{"main": "${a + b}"}' # → 32
# With a file (a.yml defines comp1, comp2):
ymx a.yml -c 'main: $comp2(x=2,y=3)\ncomp1$: a * b'  # → 6 (\n expanded to newline)
```

**Flags:**

- `--entry <component>`: name of the component within `<file>` to compile (default `main`). The project root is the file's parent directory; the entry path is derived internally as `<file_stem>.<component>` (always exactly 2 segments). If the file is missing or the entry name is not a valid component identifier, the CLI emits `E009` at option-resolution and exits non-zero. If the file exists but the component is not defined in it, the CLI emits `E002` at compile time and exits non-zero.
- `--max-depth <n>`: limit on template/call recursion (default `256`).
- `--output <file>`: write JSON to a file instead of stdout. The file is written only on success; on any diagnostic the CLI exits non-zero without creating the file.
- `--pretty`: pretty-print the JSON output.
- `--plain`: promote sub-namespace components **and** templates into the global namespace (equivalent to `_ymx.plain: true`).
- `--plain-template`: promote sub-namespace **templates only** into the global namespace (equivalent to `_ymx.plain: template`). `--plain` and `--plain-template` are mutually exclusive (CLI arg error). Each overrides the entry-file `_ymx.plain` value per the precedence rule.
- `--format <json|diagnostics>`: output style (v1: `json`; `diagnostics` lists errors only).
- `--test`: run inline `_test` cases (via `ymx-test`) instead of compiling the entry. When `path` is omitted it defaults to `.` (the current directory). When `path` is a **directory**, recursively walk it and run all `_test` blocks in every subdirectory containing `.yml`/`.yaml` files (each is an independent project); aggregate results across all projects and exit non-zero if any test fails. When `path` is a **file**, run tests for that single project (the existing behaviour). `--test` does not read stdin.
- `-c, --code <yml>`: inline YAML or JSON component definitions. Accepts both YAML and JSON formats (auto-detected, same as stdin args). Four usage modes:
  - **No file, `-c` only**: the inline content is the script; entry is `main.main` (or `--entry` override). Equivalent to `cat script.yml | ymx` but without a file on disk.
  - **File + `-c`**: load the file normally (entry derived from file stem), then overlay the `-c` components onto the global namespace. Components from `-c` **completely replace** any file component with the same name (last-registered-wins). New names from `-c` are added. The file's `_ymx` / `_test` metadata is unaffected.
  - **Stdin args + `-c`** (no file): the inline content is the script; stdin provides call arguments (JSON/YAML, same as the stdin-args mode).
  - **File + stdin args + `-c`**: load the file, overlay `-c`, stdin provides call arguments.
  `-c` does **not** contribute `_ymx` or `_test` metadata — only the file's front matter applies. `-c` combined with `--test` is a CLI usage error (exit 2).

**Exit codes.** `0` on success; `2` for CLI usage errors (missing required argument, `--stdin` with a terminal, etc.); `1` for any diagnostic produced by the pipeline (parse/namespace errors, E001/E004/…, a missing entry file, a missing entry component, max-depth, or a failing `_test` under `--test`). With `--format diagnostics` on a successful compile, stdout is empty and the exit code is `0`.

**Orchestration.** The CLI is the canonical full pipeline: `ymx_lib::load_project(path.parent())` → `ymx_config::extract_options(&project, &cli)` → `ymx_core::compile(&project, &opts)` (or `ymx_test::run_tests(&project, &opts)` under `--test`) → serialize/emit. When stdin provides call arguments (no `path` or `path` given but stdin provides args), `compile` is replaced by `ymx_core::compile_component(project, "<entry>", &args, &opts)`. The project root is derived from the file's parent directory; `--entry` is resolved before `extract_options` because it selects the front-matter source file (see *`_ymx` — front matter*). When `--test` is given with a directory path (or `.` by default), the CLI walks that directory recursively, treating each subdirectory containing `.yml`/`.yaml` files as an independent project, and runs `load_project` → `extract_options` → `parse_tests` + `run_tests` per project, aggregating results and exiting non-zero if any test fails across all projects.

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
    pub default_keyword: String,// default "default" (engine prefixes with "$" internally)
    pub max_depth: u32,         // default 256
    pub pretty: bool,           // default false
    pub format: Format,         // default Json
    pub plain: PlainMode,       // default False
    pub allowed_backends: Option<Vec<String>>,  // None = all backends allowed
    pub executor: Option<Arc<dyn CommandExecutor>>,  // None = shell execution disabled (E016)
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
}

/// Error from a failed shell command execution.
pub enum ExecError {
    UnknownBackend(String),
    SpawnFailed(String),
}

/// Executor for shell commands (`$sh{...}`, `$pw{...}`, etc.).
/// Implemented by the caller (ymx-lib provides StdExecutor); ymx-core stays I/O-free.
pub trait CommandExecutor: Send + Sync {
    fn execute(&self, backend: &str, command: &str) -> Result<ExecOutput, ExecError>;
}
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
    pub default_keyword: Option<String>,
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
/// failed load yields no `Project` and thus no tests run — see *Reach of the
/// error variant*).
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
    /* other fields: from_keyword, default_keyword, max_depth, output, pretty,
       plain, plain_template, format */
}
```

### Serialization & errors

- `Value` serializes to JSON with insertion-ordered object keys (`serde_json` + `preserve_order`). On success with `format = Json`, callers serialize `Value` (pretty or compact per `pretty`); with `format = Diagnostics`, there are no diagnostics to emit on success.
- `Err(Vec<Diagnostic>)` from `load_project`/`compile`/`extract_options`/`parse_tests` carries all errors collected during namespace resolution, front-matter validation, or compilation; the CLI renders them per the *Diagnostics* format.

## Features

**v1**

- Compile a directory of YAML files into a JSON document, applying rules 1–16.
- Configurable compile flags (see CLI).
- Compile multi-file, directory-scoped projects; file-path entry addressing (`main.main`, `a.b.c`) and namespace promotion via `_ymx.plain` / `--plain` / `--plain-template`.
- Structured diagnostics reporting file path, line, column, and component name where an issue occurred, plus an error code.
- Usable as a CLI tool and as a Rust library (`ymx-lib`).
- Inline `_test` blocks (see *Project metadata*) drive a tests-first development flow via `ymx-test`.
- Shell execution builtins (`$sh`, `$pw`) with extensible backend registry and `_ymx.allowed_backends` restriction (rule 19).

**Later**

- HTML and PDF renderers.
- WEB service (REST endpoint that compiles submitted YAML).
- Swappable math/engine backends.
- User-defined builtins via a plugin system.
- Rich "bug report" mode with full call-stack and local-argument dump.

## Testing

Tests are **first-class**: every scenario lives in `tests/cases/rule-NN/<scenario>/` as a real YMX project (one directory = project root), and assertions are written inside the YAML itself via the `_test` meta key (see *Project metadata*) — no hand-written expected-output files and no external snapshot tooling. Both success expectations (`Expected::Value`) and compile-time diagnostic expectations (`Expected::Error`) are expressed in `_test`. The test harness is a small Rust integration test that, for each scenario directory:

1. `ymx_lib::load_project(scenario_dir)` → `Project` (collecting raw `_ymx`/`_test` meta).
2. `ymx_config::extract_options(&project, &CliOverrides::default_for_tests())` → `Options` (front-matter defaults override the engine defaults; the harness sets no CLI overrides unless the scenario requires them).
3. `ymx_test::run_tests(&project, &opts)` → `Vec<TestResult>`; the harness asserts every test `passed`, printing the failing target + expected/actual diff on failure.

**Scenario layout.**

```
tests/cases/rule-NN/<scenario>/
├── main.yml        # the entry document (defines the `_test` block; may define `_ymx`; may define `_test._build_error`; defines main only if a test targets it)
├── <other>.yml     # additional documents in the same project (multi-file scenarios)
└── subdir/         # sub-namespace documents
```

- Every scenario must define at least one `_test` entry. A scenario asserts either a value (`Expected::Value`) or a diagnostic (`Expected::Error`); the `error` variant may assert codes that arise **after** a successful `load_project` — option-resolution (`E009`, the unknown/invalid-`_ymx`-field part of `E010`) and target-compilation (`E002`, `E003`, `E005`, `E006`, `E008`, the call-site / string-escape / math-identifier / mixed-shape-chain parts of `E010`, `E011`, `E012`, `E013`). Load-time codes (`E001`, `E004`, `E007`, `E015`) are not `_test`-driveable because `load_project` is all-or-nothing (see *Reach of the error variant*).
- The `_test._build_error: <code>` key asserts that `load_project` **or** `extract_options` fails with the given diagnostic code — a matching diagnostic is a PASS, a mismatch or unexpected success is a FAIL. This makes `E009` (entry not found), the invalid-`_ymx`-field part of `E010`, and all other load/option-time codes `_test`-driveable without silently skipping them. The shape mirrors `Expected::Error` from regular `_test` assertions; when `_build_error` is set, no `result`/`error` assertions may appear in the same `_test` block.
- The only diagnostics that are **not** `_test`-driveable by construction are produced by parsing the `_test` block itself (the malformed-`_test`-block part of `E010`) and YAML-parse failures (`E001`) of the document that hosts the `_test` block — both yield an unreadable `_test`. Together with the other load-time codes (`E004`, `E007`, `E015`) they are exercised by ordinary crate `#[test]` unit tests with inline YAML snippets. The test crate `ymx-test` exposes enough of `parse_tests`/`run_tests` to drive these where convenient.
- `_ymx` in a scenario's entry document sets non-default flags the rule needs (e.g. `max_depth` for an `E008` case, a custom `from_keyword` for rule 6 keyword-override scenarios, or `plain: template` / `plain: true` for namespace-promotion scenarios).
- Multi-file / namespace / file-scope scenarios add documents and subdirectories; `_test` targets must be components in the same document as the `_test` block.

## Compiling Rules

The following rules define how YMX parses and resolves components.

### String syntax

Inside a string value, `$` triggers interpolation; `\` is the escape character.

- `$name` (where `name` is `[A-Za-z_][A-Za-z0-9_]*`) references a named argument. The `name` grammar does not include `.`; namespace-qualified names (e.g. `subdir.comp`) are not reachable via bare `$name` (see *Multi-file projects*).
- `$0`, `$1`, … `$N` (where `N` is decimal digits) reference positional arguments.
- `${...}` enters math context (rule 7).
- `\$` produces a literal `$`; `\\` produces a literal `\`. Any other `\X` is a hard error (`E010`).

**Interpolation result type.** When the *entire* value of a property is a single interpolation — `$name`, `$N`, or `${...}` with no surrounding text — the result keeps the interpoland's native type (e.g. `phone: $user_phone` yields `123456789` as Int when the bound argument is an Int). When an interpolation appears with surrounding text, the result is a String in which each interpoland is rendered per the *Number→string rendering* rule below.

**Number→string rendering.** When a number is rendered into text (string interpolation, math string-concat under `+`, or a scalar surfaced inside a larger string):
- Int renders plainly: `20` → `"20"`.
- Float renders with the **same round-trippable rendering used for JSON output of `Value::Float`** (serde_json/ryu-style): integer-valued floats keep a fractional part — `2.0` → `"2.0"`, `2.5` → `"2.5"`, `0.1` → `"0.1"`. A single shared f64 renderer is used for both string interpolation and JSON output; Rust's default `{}` formatting is **not** used (it would drop the fractional part of integer-valued floats, e.g. `2.0` → `"2"`).

Bool renders as `"true"`/`"false"`; Null renders as `"null"`. Objects and arrays have no meaningful string rendering and raise `E011` when interpolated into text.

### Component visibility

By default, components and templates are visible across the entire project (global namespace). A name whose effective identifier starts with `_` is restricted to **file scope** — it is only visible from the document that defines it.

- `_a` is a file-scoped component: only callable from within the same document.
- `$_a` is a file-scoped template: only applied to matching components defined in the same document.
- The same applies to chained templates: `$$a` and `$$$a` chain through the global `a`; `$_a` / `$$_a` chain through the file-scoped `_a`.

Matching between a component and its template is unaffected — `_a` matches `$_a`, just as `a` matches `$a`. They are distinct names: a document may define both `a` (global) and `_a` (file-scoped).

Referencing a file-scoped component from outside its document is a hard error (`E005`).

**Effective identifier.** A component or template name is a leading sequence of `$` characters (the template prefix, zero or more) followed by an *effective identifier*. The effective identifier is `[A-Za-z_][A-Za-z0-9_]*` (letters, digits, underscores; must start with a letter or underscore). A leading `$` count of zero marks a regular component; one or more mark templates of increasing chain depth. The `_`-prefix check for file scope applies to the effective identifier (after stripping any leading `$`s): `$_a` is file-scoped because its effective identifier `_a` starts with `_`.

**Reserved names.** Two kinds of effective identifiers are reserved and not user-definable as components/templates:

1. **Builtin names** — `map`, `reduce`, `merge` — used by `$map`, `$reduce`, `$merge` (rules 15–16). Defining any component or template whose effective identifier is one of these is a hard error (`E007`), regardless of the leading `$` count. The builtins are always invoked via their `$`-prefixed forms.
2. **Meta keys** — `_ymx` (front matter) and `_test` (tests) — described in *Project metadata*. When present at the top level of a document they are intercepted by the engine as metadata and are **never** registered as components or templates. They are not file-scoped components despite their leading `_`; the `_`-prefix visibility rule simply does not apply to them. The bare top-level keys `_ymx` and `_test` are **consumed** (no error); a user component/template cannot be named `_ymx` or `_test` — any such top-level key is treated as the meta block of that name. A component or template whose **effective identifier** equals `_ymx` or `_test` but which carries one or more leading `$` (e.g. `$_ymx`, `$_test`, `$$_ymx`) is **rejected as a reserved name** (`E015`): only the bare meta keys are consumed, and the leading-`$` variants are not legal user-defined components/templates. Meta extraction is performed by `ymx-config` (`_ymx`) and `ymx-test` (`_test`); `ymx-core` only recognizes the two names, excludes them from the namespace, and stores their raw parsed values on the `Project`.

> A namespace dot (`.`) appears only at the *lookup* layer for `from` targets and math `name(...)` calls (e.g. `from: subdir.comp`); it is not part of an effective identifier and cannot appear inside `$name` interpolation. Cross-namespace component references are reached via `from` (rule 6) or via the math `subdir.comp(...)` form (rule 7), never via bare `$subdir.comp` (which would interpolate `$subdir` then the literal text `.comp`).

**Property-key modifiers.** A *property key* (the left-hand side of a property, inside a component body) may carry one or two trailing modifiers that affect how the property is resolved — they are **stripped before** the property name is used for resolution, matching, or the rule-8 shortcut, so `x?` and `x$` both target the slot named `x`.

- `?` (optional / default-merge — rule 17, **v1**): `x?: v` declares a default `v` for `x` and activates object-merge mode for the enclosing component.
- `$` (math shorthand — rule 18, **v2**): `x$: src` ≡ `x: ${src}` — the value is a math source string, evaluated to produce the property value.
- Combination `?$` (**v1**): `x?$: src` ≡ `x?: ${src}` — an optional property whose default is a math-evaluated expression. `x$?:` is `E010` (wrong order).
- The modifiers apply to **all property keys** — ordinary string names, integer positional keys (`0?:`, `1$:` ⇒ default for / math-evaluate `$0`, `$1`), and the template/component-name position (the leading name of a top-level pair: `a$: src` ≡ `a: ${src}`). They do **not** apply to the reserved meta fields `_ymx`/`_test` nor to any field *inside* those meta blocks: those are not callable components (no template, no `from`, no `${...}`, no `$N`) and the modifiers are rejected on them (`E010`).

### 1. Top-level keys are components

Every key-value pair in the main document is a component. The key gives the component's name; the value gives its content.

### 2. Components are callable with arguments

Arguments are referenced in a component body by a name starting with `$` (the name may contain `_`).

Example:

```yml
user:
  name: $user_name
  phone: $user_phone
```

Calling `user` with `user_name="Mathew"` and `user_phone=123456789` produces the object `{"name": "Mathew", "phone": 123456789}`.

> Note: argument values are parsed, falling back to string when no other type matches.

> Argument values are parsed at the call site (numbers/strings/null/bool) before being bound to `$N` / `$name` inside the called component.

> Bare `$name` (no parens) resolves to the named argument `name` if it is in scope; otherwise it is a hard error (rule 10, `E003`). This applies wherever `$name` appears, including inside plain strings. `$name(...)` unconditionally calls the component `name` and bypasses the argument lookup. Use `$name()` (with empty parens) to call a component without arguments.

### 3. Components can be called inline using `$`

A `$name(...)` expression inside a value calls another component instead of reading a property.

Example:

```yml
a: $b(x=12,y=34)
b: $x + $y
```

Here `$b` is treated as a call to the `b` component from inside `a`'s body, rather than as a property of `a`.

> A `$b(...)` call may mix positional and named arguments, e.g. `$b(12, y=34)`. Positional arguments bind to `$0`, `$1`, …; named arguments bind to `$<name>`.

> `$name(...)` unconditionally calls the component `name`, even if a `name` argument is in scope. Use `$name(...)` to bypass the argument lookup that bare `$name` performs (rule 2). Inline `$comp(...)` calls run during step 1 of rule 11 — before templates (rule 5) and before `from` dispatch (rule 6).

> Call-site argument grammar. The `(...)` of an inline `$name(...)` (rule 3) or math `name(...)` (rule 7) call holds a comma-separated argument list. Each argument is either positional `value` or named `key=value` (where `key` is an effective identifier). Positional args bind to `$0`, `$1`, …; named args bind to `$key`. Mixing positional and named is allowed, but a positional arg may not appear *after* a named one (hard error `E012`); `()` calls with no arguments. The call target of an inline `$name(...)` is a single effective identifier (namespace-local); cross-namespace calls go through the math `name(...)` form, which accepts a dotted path (rule 7), or through `from` (rule 6).

> Argument value parsing. An argument value token is parsed, in order, as Null (`null`/`~`), Bool (`true`/`false`), Int (integer literal), Float (decimal or exponent literal), or String; unquoted text that matches none becomes a String. Single- or double-quoted tokens are always Strings. Argument values may contain nested `$call(...)` and `${...}` (resolved as nested call-sites per rule 11). A direct argument value that is an inline YAML array/object literal (e.g. `$b([1,2])` or `$b({x:1})`) is a hard error in v1 (`E013`); build structured arguments with a `from`-mini-component instead.

### 4. Positional arguments are supported with `$0`, `$1`, `$2`, …

When calling a component with `$`, arguments may be unnamed. They become sequence properties `$0`, `$1`, `$2`, … inside the called component.

Example:

```yml
a: $b(12,34)
b: $0 + $1
```

Calling `a` returns `"12 + 34"` again.

> **Integer property keys are positional slots.** A property whose YAML key is the integer `0`, `1`, `2`, … (a non-negative integer scalar, not the string `"0"`) denotes the positional slot `$N` of the same index: it sets/reads `$0`, `$1`, … exactly. This generalizes rule 11's `0: $x` mini-component usage and the binding above — a component body may provide a default `$N` by writing the integer key, and a call may set a positional slot via the integer key. A string key `"0"` is an ordinary named property, distinct from the integer key `0`. Negative or non-integer keys are ordinary named properties.

### 5. Components can have template components

A component whose name starts with `$` is a template. When a component with a matching (non-`$`) name is called, the template is applied afterwards automatically.

> Templates are **namespaced by default**: a `$box` defined in `subdir/` applies only to components in the `subdir` namespace. A template whose effective identifier starts with `_` (e.g. `$_a`) is restricted to file scope (see *Component visibility*). The `_ymx.plain` flag (CLI `--plain` / `--plain-template`; default `false`) promotes sub-namespace names into the **global** namespace — `true` promotes components **and** templates; `template` promotes templates only. Promoted names participate in global template lookup, bare `$name`, rule-8 shortcut, and `from` resolution. A promoted name that collides with an existing global definition is `E004`; the namespaced qualified path (e.g. `subdir.comp`) remains reachable alongside the promoted bare name. Template-chain lookup (`a` → `$a` → `$$a` → …) consults the component's own namespace first, then global (per `plain`); a broken link stops the chain (a component does **not** skip a missing `$a` to reach `$$a`).

```yml
$box:
  from: div
  children: Hello, $name!
box:
  name: Sir. $name
```

Calling `box` with `{"name": "Rocky"}` produces `{"from": "div", "children": "Hello, Sir. Rocky"}`. The argument `name="Rocky"` is applied to `box`; `box` then invokes `$box`, which expects a `$name` property.

Templates can be chained indefinitely: a component `a` invokes `$a`, which itself can have a template `$$a`, which can have `$$$a`, and so on. The chain unwinds in order — the innermost template is applied first, then its result feeds the next template, until no more templates match.

> Templates can only be reached through their **direct** child: `a` invokes `$a`; if `$a` is absent, `a` does **not** skip to `$$a` — the chain is broken at that point. A template name (starting with `$`) is not a valid `from` target. Template application is step 2 of rule 11 — it sits between property resolution (step 1, where inline `$comp(...)` calls run) and `from` dispatch (step 3).

When a component's result is a scalar (not an object with named properties), it is passed to the next template as the positional argument `$0`, consistent with rule 4. The full argument-passing rule for scalar, object, and array results — including how the *initial* arguments are retained — is specified just below.

**Argument passing between chain steps.** The arguments the *next* template sees are derived from the *initial* arguments (the args the original component was called with), not the previous template's full arg set:

- **Scalar result** → the next template receives the initial arguments with `$0` overwritten by the scalar (other initial keys retained). The scalar does not consume other initial keys.
- **Object result** → the next template receives the initial arguments, **overwritten only** for the keys the object actually returns, and **only** for the immediately next chain step. After that step, the chain reverts to the initial arguments (the overwrite does not propagate further up the chain). A key returned by the template that was not in the initial args is added for that one step only.
- **Array result** → rules 12–14 govern; an array result propagating up a *non-array* chain link is the "mixed-shape" case below.

This mirrors the overwrite-then-revert semantics of rule 13's array reduce, applied one chain link at a time. If the next template references an arg that is neither in the initial set nor overwritten by the current result, it is `E003` (rule 10).

```yml
a:
  x: 1
  y: 2
$a:
  x: ${x + 10}     # returns {x: 11} (only x)
$$a:
  out: $y          # y is still in the initial args → "2"
```

Calling `a`: `$a` returns `{"x": 11}`; `$$a` sees the initial `{x:1, y:2}` with `x` overwritten to `11` for this step, so `$y` → `2`, producing `{"out": 2}`. (Without the overwrite-then-revert rule, `$a`'s `{x:11}`-only result would make `$y` in `$$a` an `E003`.)

Example

```yml
$$$a: "final: $0"
$$a:  $0 + 1
$a:   $0 * 2
a:    10
```

Calling `a`:

1. `a` returns `10` to its template `$a` (initial args here are empty, `$0` is the only slot).
2. `$a` runs `$0 * 2` with `$0 = 10`, returning `20` to `$$a`.
3. `$$a` runs `$0 + 1` with `$0 = 20`, returning `21` to `$$$a`.
4. `$$$a` runs `"final: $0"` with `$0 = 21`, returning `"final: 21"`.

So calling `a` returns `"final: 21"`.

> **Mixed-shape chains (v1 limitation).** A single chain whose links mix the array shape (rules 12–14) with the non-array shape (rule 5) is **not defined in v1** and raises `E010` when the mismatched link is reached — e.g. `$a` is non-array but `$$a` is array, or `$a` returns an array into a non-array `$$a`. The supported chains are: all links non-array (rule 5), or a terminal array-`$a` applied to a component via rules 12/13/14. This is a documented gap pending a concrete use case; revisit in a later version.

> **Merge-mode templates (v2).** A template whose body contains at least one `?:` property activates rule 17's object-merge mode: the caller's supplied properties forward into the output and win over the template body, while the body (and its `?:` defaults) fills the gaps. A template with no `?:` property keeps the rule-5 behavior above (its output is its own resolved body).

### 6. Components can call each other with the `from` property

The `from` property references another component by name. Users can override this keyword in their own context to avoid conflicts.

Example:

```yml
CompA:
  from: CompB
  x: 12
  y: 34
CompB: $x + $y
```

Calling `CompA` returns `"12 + 34"`.

> If a component has both `from` and a matching `$template`, the template chain applies FIRST, then `from` resolves against the template's result (see rule 11).

> The `from` value is computed as part of property resolution (step 1) and may be any expression that resolves to a component name — e.g. `from: $b()` first evaluates `$b()`, then uses its return value as the `from` target.

> If the (resolved) `from` value does **not** name a valid *regular* component, `from` is treated as a plain property — no call is made and no error is raised. Template names (those starting with `$`) are not valid `from` targets; templates are only reached through the automatic chain (rule 5). Namespace-qualified targets are supported: `from: subdir.comp` resolves `comp` in the `subdir` namespace (see *Multi-file projects*). If the resolved `from` value is not a String (a number, bool, null, array, or object) it is also treated as a plain property — no call, no error — consistent with the invalid-target rule above.

Example — invalid `from` is a plain property:

```yml
a:
  from: b
```

Calling `a` (no `b` component defined) returns `{"from": "b"}`. Adding component `b`:

```yml
a:
  from: b
b: 123
```

Now `a` calls `b` and returns `123`.

### 7. Math and component calls with `${...}`

The `${...}` form evaluates its contents as a math expression and can also call components as functions inside it.

Example:

```yml
a: $b(12,34)
b: ${$0 + $1}
```

Calling `a` returns the number `46`.

Components can also be called inside the expression:

```yml
a: ${b(12,34) + c(28)}
b: ${$0 + $1}
c: ${2 * $0}
```

Here `a` calls `b` which sums `12` with `34` yielding `46`, then calls `c` with `28` which doubles it to `56`. Finally `a` sums them to `102`.

> The math operators are:
> `+` (Addition): Sums two numbers, or (when either operand is a non-numeric String) concatenates — see `+` semantics below.
> `-` (Subtraction): Subtracts the right value from the left (numeric only).
> `*` (Multiplication): Multiplies two values (numeric only).
> `/` (Division): Always floating-point division; the result is a Float. `5 / 2` → `2.5`. Division by zero is `E011`.
> `%` (Remainder/Modulus): Integer remainder of `left % right`, with both operands coerced to Int (Floats truncated toward zero, non-numeric → `E011`). Sign follows the dividend.
> `**` (Exponentiation): Raises the first operand to the power of the second. `Int ** Int(non-negative)` → Int; everything else → Float. Negative or fractional exponents are Float.

> Precedence (highest to lowest): `**` (right-associative) > unary `-` > `* / %` (left-associative) > `+ -` (left-associative). Parentheses group. There are **no** comparison, equality, or boolean operators; `<`, `>`, `=`, `==`, `and`, `or` are literal text in strings (see rule 13's `$x + $y < $last`, which yields the String `"1 + 2 < 6"`).

> `+` semantics: the operands are first *resolved* (per *Math operand resolution* below). Then: if both resolved operands are numbers (Int or Float), numeric addition (promoted to Float if either is Float); if both are Strings, string concatenation; if exactly one is a String and the other is a number, the number is rendered per *Number→string rendering* and string-concatenated. Any other mixture (Bool, Null, Array, Object) is a hard error (`E011`).

> Numeric promotion: Int ⊕ Int = Int (except `/`, always Float, and `**` per above); any Float operand promotes the result of an arithmetic operator to Float. Bool and Null are not numbers and cannot be coerced.

> `${...}` return type: the math expression may evaluate to any `Value`, not just numbers. `${ $0 }` returns the argument unchanged; `${ x + y }` returns a String when `+` concatenates; `${ obj }` (referencing an in-scope object argument) returns that object. The value flows into surrounding interpolation per *String syntax: Interpolation result type*.

> Identifier grammar inside `${...}`. Named arguments are written as **bare identifiers** (`x`, `y`, `default`, `last`, …) *without* a `$` prefix; positional arguments are written `$0`, `$1`, … (a `$` followed by decimal digits); components are called as `name(...)` *without* a `$` prefix, with `name` optionally a dotted namespace path (e.g. `subdir.comp(12,34)`) and `name()` calling a no-arg component. A `$` followed by a letter inside `${...}` (e.g. `${ $x }`) is `E010` — drop the `$` to reference a named argument. A bare identifier resolves to the in-scope argument of that name or to `last`; anything else is `E003`.

> String literals in math (v1): `${...}` does **not** accept quoted string literals in v1; string operands come only from in-scope argument references (which may themselves be Strings, subject to *Math operand resolution* below). A future version may add string literals inside math.

> Math operand resolution (String re-scan). When an operand of a math operator — a bare identifier, `last`, or a `$N` positional — resolves to a **String**, that String is re-scanned as a math expression and evaluated **in the current scope** (the same scope as the enclosing `${...}`, including `last` and all in-scope arguments): `"1 + 2"` → `3`, `"123"` → `123`, `"x + 1"` (with `x` in scope) → `x + 1`. If the String does **not** parse as a math expression (e.g. free text like `"hello world"`), the identifier is left as a plain String operand of the surrounding operator (numeric operators then raise `E011`; `+` concatenates). Non-String operands are used directly. This re-scan is what makes `last` work (rule 16's `${last}` with `$last = "1 + 2"` yields `3`); it applies uniformly to *every* String-valued operand in math, not only to `last`.
>
> Gotcha: because re-scan evaluates in the current scope, a String argument whose content is a bare-identifier-looking token resolves to that identifier. E.g. `${ x }` with `x = "y"` re-scans as `y`, which looks up the argument `y` (→ `E003` if absent). Keep String arguments used in math either numeric or full math expressions; avoid re-using argument names as string contents.

> `last` is available in `${...}` only within a reduce step (rules 13–16). Outside any reduce step — including on the **first** step of a reduce, before any previous result exists — referencing `last` (or `$last` in a plain string) is `E003` (treated as a missing argument; `last` is an ordinary in-scope argument, nothing more). `last` and `$last` are thus **symmetric across reduce contexts**: both the array-template reduce (rule 13) and `$reduce` (rule 16) expose the previous step's result, accessed as `last` in math and `$last` in plain strings.

### 8. Shortcut: a property name matching a component name calls that component

If a component defines a property whose name matches another component, that property value is passed to the matched component as `$default`, and the remaining properties of the calling component are passed as arguments.

Example:

```yml
a:
  b: 1
  y: 3
  z: 5
b: [$default,$y,$z]
```

Calling `a` returns `[1, 3, 5]`. The leading value passed through `$default` corresponds to the property named after the target component; its name is configurable.

> If more than one property's name matches a component, it is a hard error (ambiguous shortcut).

> The shortcut fires during step 3 of rule 11 — against the post-template property set, as sugar for `from` (the two are mutually exclusive). The shortcut is **suppressed** when the component has a `from` property pointing to a valid regular component: in that case `from` is the call directive, and the otherwise-matching property is passed as a regular argument to the `from`-targeted component. The shortcut applies inside nested mini-components under the same conditions, including the same suppression when a nested `from` is valid. Suppression does **not** happen when `from` is invalid — in that case `from` is forwarded as a **normal property** alongside the other arguments to the shortcut-matched component, and the shortcut fires normally.

Example 1 — shortcut fires:

```yml
a:
  b: 1
b: ${default + 1}
```

Calling `a` returns `2`.

Example 2 — shortcut suppressed by a valid `from`:

```yml
a:
  from: c
  b: 1
b: ${default + 1}
c: ${b + 2}
```

Calling `a` returns `3`: `from: c` is valid, so `a` calls `c` with `b=1` (rule 8 does not fire).

### 9. Non-existing properties in the calling component are ignored

```yml
a: $x + $y
```

Calling `a` with `{"a":1,"b":2,"c":3}` returns `"1 + 2"`; the `c` property is ignored because `a` does not reference it.

### 10. All referenced properties are required

Any property referenced by a component (via `$name`, `$0`, `${name}`, etc.) must be supplied when the component is called. Unknown/extra properties are ignored per rule 9.

### 11. Components are resolved in a fixed three-step order

Resolving a component runs in three steps, in this fixed order:

1. **Property resolution (before template)** — every property value of the component is fully resolved. A property value is a *nested call-site* when it is an object containing the `from` key, or any value containing an inline `$comp(...)` call (rule 3) or a `${...}` interpolation (rule 7). Nested call-sites resolve **bottom-up**: the deepest nested call is evaluated first, its return value bubbles up to its parent, and so on, until every property of the component has a fully resolved value. Bare `$name` (no parens) resolves as: (a) a named argument `name` in scope → that argument's value; (b) else → hard error (rule 10, `E003`). Use `$name()` (with empty parens) to call a component without arguments. `$name(...)` unconditionally calls the component `name` (rule 3) and bypasses the argument lookup. Inside `${...}` (math context) there is **no fallback**: a bare identifier refers to an argument or the math result of the previous step (`last`); to call a component inside math, use the `name(...)` form (rule 7). *(v2 only for `$`; `?` is v1)* Property-key modifiers (`?`, `$`) are stripped first; a `$`-suffix value is wrapped in `${...}` and a `?:` default is recorded for later merge binding (rule 17).
2. **Template chain (rule 5)** — applied to the post-step-1 property set. The innermost template runs first, its result feeds the next template, and so on. Each template link is itself a **normal component call** and follows this same three-step flow (its own property resolution, its own template chain, its own `from`/shortcut dispatch), **with one exception**: the *first* link of a chain whose `$template` is an array uses the rule 12/13/14 map/reduce semantics instead of a single call. Templates can only be reached through their **direct** child: `a` invokes `$a`; if `$a` is absent, `a` does **not** skip to `$$a` — the chain is broken at that point. Template names are not valid `from` targets.
3. **`from` / shortcut dispatch (rules 6 and 8, after template)** — these are **mutually exclusive and sugar-equivalent**: the rule-8 shortcut is sugar for `from`, so exactly one of them fires. If the (post-template) value of `from` names a valid *regular* component, `from` dispatches: the target is called with the rest of the property set as arguments (the `from` key itself is **not** forwarded; the rule-8 shortcut is **suppressed**), and the return value replaces the component's output. If `from` does **not** name a valid regular component (templates excluded; non-String `from`; missing target), `from` is a **plain forwarded property** and the rule-8 shortcut fires normally against the post-template property set (a property whose name matches a component → that component is called with the remaining properties as arguments, the matched key's value passed as `$default`; the invalid `from` is forwarded alongside them as an ordinary argument). Either dispatch target is a normal component call following this same three-step flow.

A nested mini-component (an object whose value uses `from`) receives **only the arguments explicitly written in its body**, resolved against the parent's current arguments. The parent's other arguments are not auto-forwarded; rules 9 and 10 apply within the nested call exactly as they do at the top level. A nested mini-component follows the same three-step evaluation as a top-level component, so it can itself contain nested call-sites.

Inline `$comp(...)` calls (and `${...}` interpolations) run in **step 1**, before templates. `from` dispatch runs in **step 3**, after templates. Recursion (nested calls, template chains, `from` dispatch) is bounded by `--max-depth`; the depth counter is checked and incremented on entry to each such operation per *Architecture: Cycles*; exceeding it raises `E008`.

Example 1 — nested call-sites resolve inner-to-outer:

```yml
a:
  from: b
  x: $compC($x)
  y:
    from: compC
    0: $x
  z:
    from: compD
    a:
      from: compE
      ...
```

Evaluating `a`, assuming each `comp*` and `b` resolves to a valid component:

1. Resolve the deepest nested call first: `z.a.from` invokes `compE` with the args written in that nested object (resolved against `z`'s context). Its return value becomes property `a` inside `z`.
2. With property `a` resolved, `z.from` invokes `compD` with the args written in `z`'s body (including the now-resolved `a`).
3. Independently, `y.from` invokes `compC` with `0 = $x` (resolved against `a`'s args).
4. Independently, `x` evaluates the inline `$compC($x)` call.
5. `a` now has fully resolved values for `x`, `y`, and `z`. Its template chain (if any) runs next, then `a.from` invokes `b` using those resolved values as arguments.

Example 2 — inline calls run before templates; `from` runs after:

```yml
a:
  from: $b()
$a:
  from: $from
  x: 2
b: c
c: ${1 + x}
```

Calling `a`:

1. Property resolution: `from: $b()` calls `b` → `"c"`. `a`'s args are now `{from: "c"}`. (`from: $b` would be equivalent here, since no `b` argument is in scope.)
2. Template `$a` runs with those args: `{from: $from, x: 2}` → `{from: "c", x: 2}`.
3. `from` dispatch: `from="c"` names a valid regular component → call `c` with `{x: 2}`.
4. `c` returns `${1 + 2}` = `3`.

Final result: `3`.

### 12. An array component maps over its template

When a component is an array and a matching `$template` exists, each item of the array is passed through the template, producing one output item per input item.

Example 1

```yml
$a:
  prop1: ${x + 1}
  prop2: ${y * x}
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` produces `[{"prop1": 2, "prop2": 2}, {"prop1": 4, "prop2": 12}]`.

Example 2

```yml
$a: $x + $y
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` produces `["1 + 2", "3 + 4"]`.

### 13. An array template component reduces over its sibling component

When the template is an array, the sibling component supplies the initial arguments. The template iterates over its own items; on each step the previous item's result is available as `$last`.

Argument overwrite rule: each step starts from the **initial** arguments. The previous step's result **only** overwrites the initial for the **immediately next** step, and **only** for the keys it actually returns. If the previous step returned a non-object (a number, a string, an array, …), no overwrite happens and the next step reverts to the initial arguments.

Example

```yml
a:
  x: 1
  y: 2
$a:
  - x: ${x + 1}
    y: ${y + 2}
  - ${x + y}
  - $x + $y < $last
```

Calling `a`:

1. The first template item runs with the initial `x=1, y=2`, producing `{"x": 2, "y": 4}`. This object returns `x` and `y`, so it overwrites the initial for the next step **only**.
2. The second item runs `${x + y}` with `x=2, y=4` (overwritten), producing the number `6`. Since it returns a number (not an object with `x`/`y`), no overwrite carries forward.
3. The third item therefore reverts to the **original** initial values `x=1, y=2`, with `$last=6`, producing the string `"1 + 2 < 6"`.

So calling `a` returns `"1 + 2 < 6"`.

### 14. When both the component and its template are arrays, reduce each element independently

When both `a` and `$a` are arrays, `$a` is treated as a reduce sequence (per rule 13) that is applied independently to **each element** of `a`. The result is an array with one reduced entry per element of `a`. Each element of `a` starts its own reduce run with that element's properties as the initial arguments, using the same overwrite and `$last` semantics as rule 13.

Example

```yml
$a:
  - {sum: ${x + y}, x: 0}
  - ${sum + x + y}
  - a: $x
    b: ${2*y}
    sum: $last
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` runs a three-step reduce of each element through `$a`:

1. The first element `a[0]={x:1, y:2}`:
   - Step 1: `{sum: ${x + y}, x: 0}` runs with the initial `x=1, y=2` → `{"sum": 3, "x": 0}`. This returns an object with `sum` and `x`, so `x` (and `sum`) overwrite the initial for the next step **only**.
   - Step 2: `${sum + x + y}` runs with `x=0` (overwritten), `y=2` (initial), `sum=3` → the number `5`, which becomes `$last`. Since the result is a non-object, no overwrite carries forward.
   - Step 3: `{a: $x, b: ${2*y}, sum: $last}` runs with the original `x=1, y=2` and `$last=5` → `{"a": 1, "b": 4, "sum": 5}`.
2. The second element `a[1]={x:3, y:4}` runs the same three-step reduce:
   - Step 1: `{sum: ${x + y}, x: 0}` → `{"sum": 7, "x": 0}` (overwrites `x`, adds `sum`).
   - Step 2: `${sum + x + y}` with `sum=7, x=0, y=4` → `11` (becomes `$last`). Reverts.
   - Step 3: `{a: $x, b: ${2*y}, sum: $last}` with `x=3, y=4` and `$last=11` → `{"a": 3, "b": 8, "sum": 11}`.

So calling `a` produces `[{"a": 1, "b": 4, "sum": 5}, {"a": 3, "b": 8, "sum": 11}]`.

#### Edge cases (lenient)

The following lenient fallbacks apply to rules 12–14:

- An **empty array** `a` produces an **empty array** output.
- An **empty `$a` template** is a **pass-through**: it returns its input unchanged.
- A **non-array `$a`** applied to a **non-array `a`** simply calls `$a` with `a` as `$0` (per rule 5's chain semantics).
- An **array `$a`** applied to a **non-array `a`** (a scalar or an object) reduces `$a` over `a` as a **single-element sequence** per rule 13 — an object `a` supplies the initial arguments, a scalar `a` binds `$0`. (The mixed-shape gap of rule 5 concerns array vs non-array links *within* a single chain and remains `E010`; this case — an array template applied to a non-array component — is defined.)

### 15. Builtins: `$map`, `$reduce`, `$merge`

The three builtins are **special forms**: each declares its own argument-evaluation strategy, rather than uniformly receiving all arguments pre-evaluated. All three are invoked only via their `$`-prefixed forms; user components/templates named `map`/`reduce`/`merge` are rejected (`E007`).

#### `$map(callable, array)`

**Syntax:** `$map(<component>, <array>)`

**Semantics:** applies `<component>` to each item of `<array>`, returning an array of results. The second argument must be an Array (`E011` if not); an empty array yields an empty result.

**Argument-evaluation strategy:**
- The first argument (`<component>`) is kept **unevaluated** — it is a reference to a component to call per item, not a pre-evaluated value.
- The second argument (`<array>`) is evaluated **eagerly** (per bare `$name` resolution).

**Item binding:**
- An **object** item supplies named arguments (its keys → `$<key>`).
- A **scalar** item binds `$0`.
- An array item is `E011`.

```yml
double: ${$0 * 2}
items: [1, 2, 3]
result: $map($double, $items)
# result → [2, 4, 6]
```

```yml
add_props: $a + $b
rows:
  - {a: 1, b: 2}
  - {a: 3, b: 4}
result: $map($add_props, $rows)
# result → ["1 + 2", "3 + 4"]
```

The callable may be namespace-qualified:

```yml
result: $map(utils.trim, $names)
```

#### `$reduce(callable, array, init?)`

**Syntax:** `$reduce(<component>, <array>, <init>?)`

**Semantics:** applies `<component>` iteratively over `<array>`, accumulating a result. The final result is the return value of the last iteration. An empty array yields `Value::Null`.

**Argument-evaluation strategy:** identical to `$map` — first arg unevaluated (callable), second arg eager. `<init>` (third argument) is **eagerly evaluated** if present.

**Item binding:** identical to `$map` — object item → named args, scalar item → `$0`.

**`$last` semantics:** the result of the previous iteration is available as `$last` in the next. If `<init>` is supplied, `$last` is bound to `<init>` on the **first** iteration. If `<init>` is absent, `$last` is **undefined on the first iteration** — referencing it there is `E003`. Inside `${...}` math context, `last` (bare identifier) refers to `$last` and is subject to the *Math operand resolution* re-scan rule: a string previous result is re-evaluated as a math expression; a number is used directly; an object/array in a numeric context is `E011`.

**Consequences:**
- Single-element array + `init`: first step runs with `$last = init`
- Single-element array + no `init`: first step runs with no `$last` (E003 if callable references `$last`)
- Multiple items + `init`: step 1 has `$last = init`, step 2 has `$last = result_of_step1`, etc.
- Multiple items + no `init`: step 1 has no `$last`, step 2+ have `$last = prev_result`
- Empty array → always `Null` (no step runs)

```yml
inc: ${last + $0}
nums: [1, 2, 3]
result: $reduce($inc, $nums, 0)
# result → 6  (step 1: last=0, $0=1 → 1;  step 2: last=1, $0=2 → 3;  step 3: last=3, $0=3 → 6)
```

Using math re-scan with string results (no `init`):

```yml
a: $a + $b
b:
  - {a: 1, b: 2}    # first: returns "1 + 2"
  - $last = ${last}  # second: math-evaluates "1 + 2" → 3; result "1 + 2 = 3"
c: $reduce($a, $b)
# result → "1 + 2 = 3"
```

#### `$merge(a, b)`

**Syntax:** `$merge(<value>, <value>)`

**Semantics:** merges two values. Arrays are concatenated (first then second); objects are shallow-merged (second overwrites first for shared keys, second's unique keys are appended). Any other shape combination (Object + Array, Array + Object, scalar + anything) is `E011`.

**Argument-evaluation strategy:** **both** arguments are evaluated eagerly (no lazy callable).

```yml
a: [1, 2, 3]
b: [4, 5, 6]
c: $merge(a, b)
# c → [1, 2, 3, 4, 5, 6]
```

```yml
defaults: {theme: dark, lang: en, debug: false}
overrides: {debug: true, log: verbose}
config: $merge(defaults, overrides)
# config → {theme: dark, lang: en, debug: true, log: verbose}
```



### 17. Optional properties (`?`) — default-value object merge

A property key of the form `<name>?` declares an **optional** property `<name>` with a default value placed where the value normally goes. When the enclosing component is *called* (template application — rule 5, `from` dispatch — rule 6, inline `$name(...)` — rule 3, or `$map`/`$reduce` — rule 16), the caller-supplied value for `<name>` overrides the default; if the caller supplies no `<name>`, the default `v` is used and the property is emitted with `v` as its value. This applies uniformly across every call site.

```yml
$a:
  x?: 2
a:
  x: 1
```
Calling `a` → `{"x": 1}` (caller's `x=1` overrides the default `2`).

```yml
$a:
  x?: 2
a:
  y: 1
```
Calling `a` → `{"y": 1, "x": 2}` (caller supplies no `x`, so `?:` default `2` fills the gap; the caller's `y` still appears).

> **Merge mode is opt-in.** Object-merge forwarding activates only when the callee body contains **at least one `?:` property**. In merge mode: the caller's supplied properties (a) **win** over the callee body for the keys they name, and (b) **forward into the output** even for keys the callee body does not mention. The callee body — plain properties and `?:` defaults alike — fills the gaps for keys the caller did not supply. A callee with **no** `?:` property keeps the existing rule-5 / call behavior (its output is its own resolved body; caller props are inputs only and are *not* forwarded). This keeps v1 templates like `$box` unchanged — they emit only their own body keys, with no caller-arg leak.

> **Caller always wins.** When the caller supplies a key, the callee's plain value for that key is ignored and the caller's value is emitted. `?:` defaults for that key are likewise ignored. Template body props (including non-`?:` plain values) only ever fill gaps the caller left open. So `$a: {x?:2, z:99}` called with `{y:5, z:7}` → `{"x":2, "z":7, "y":5}`.

> **Scalars carry no keys.** When the caller passes a scalar (a number, a string, `null`, …) as the input — including `a: null` — the input has no keys to override the body with and none to forward, so the callee body (and all `?:` defaults) forms the output:

```yml
b:
  x: 1
  y: 2
a:
  from: b
```
Calling `a` → `{"x": 1, "y": 2}` (`a` has `from: b`; `b` supplies `x`,`y`).

```yml
$a:
  x?: 1
  y?: 2
a: null
```
Calling `a` → `{"x": 1, "y": 2}` (`null` carries no keys, so both `?:` defaults apply).

> **`?:` relaxes rule 10.** A property declared with `?:` is optional: referencing `$<name>` inside the component is never `E003` — it resolves to the caller-supplied value when present, or to the default `v` when omitted, and that value is emitted as the property. A referenced property with **no** `?:` default still follows rule 10.

```yml
a:
  x?: 2
  y: ${x + 2}
  z: "Value: $x"
```
Calling `a` with no `x` → `{"x": 2, "y": 4, "z": "Value: 2"}`. Calling `a` with `x=10` → `{"x": 10, "y": 12, "z": "Value: 10"}`.

**`?$` — optional property with a math-evaluated default.** `<name>?$: <src>` is exactly `<name>?: ${<src>}` — the default is computed by evaluating `<src>` as a math expression only when the caller omits `<name>`.

```yml
$button:
  label?$: "Click me"       # evaluated as math only when label is absent
  count?$: count + 1         # count must be in scope; evaluated when absent
  disabled: false

counter:
  count: 0

main: $button
# → {label: "Click me", count: 1, disabled: false}
# (count=0 supplied; count?$ is skipped because count was provided)
```

> `?$` evaluates the math expression in the callee's scope — `count + 1` sees the caller's `count`. If `count` is not in scope and not supplied, `$count` in the math expression is `E003`. The math value may be any type (not restricted to numbers); the result becomes the property value.

> **Lazy defaults.** A `?:` default `v` is evaluated (interpolation, math, inline `$call(...)` resolved) **only** when the caller did **not** supply `<name>`. If the caller supplies the key, `v` is never evaluated — dead work in unused defaults is skipped, and errors in an unused default do not surface. The plain (non-`?:`) properties of the callee are evaluated as usual during step 1 of rule 11.

> **Scope of `?:`.** `?:` applies to ordinary property names and to integer positional keys (`0?:` ⇒ default for `$0`, `1?:` ⇒ default for `$1`, …). The `?$` combination (`0?$:` ⇒ optional slot with math-evaluated default) is also v1. `?:` does **not** apply to the reserved meta fields `_ymx`/`_test` or to any field *inside* those meta blocks; a `?:` on such a key is `E010` (see *Property-key modifiers*).

### 18. Math shorthand (`$` suffix) *(v2)*

A trailing `$` on a property key (or on the leading name of a top-level component/template) is shorthand for wrapping the value in a `${...}` math expression.

**On a property** — `<name>$: <src>` is exactly equivalent to `<name>: ${<src>}`:

```yml
a:
  sum$: x + y
# same as
a:
  sum: ${x + y}
```

The value `<src>` must be a **String** (a math source). A non-string value to a `$`-suffix property is `E010` (e.g. `sum$: [1,2]`).

**On a component or template name** — `<name>$: <src>` is exactly equivalent to `<name>: ${<src>}`: the body string `<src>` is the math source, and the component's **output is the math result**. As a scalar result it feeds rule 5's template chain as `$0` to the next template, exactly like `a: 10` feeds `10` as `$0` to `$a`.

```yml
a$: x + y
# same as
a: ${x + y}
```

> **Leading vs trailing `$`.** A *leading* `$` is the template prefix (rule 5): `$$a` is a depth-2 template, `$$$a` depth-3, etc. A *trailing* `$` is the math shorthand of this rule. The two are independent: `$$a$: src` is a depth-2 template whose body is the math source `src`, equivalent to `$$a: ${src}`.

> **Combination with `?`.** The modifiers combine in the fixed order **`?$`** (optional `?` first, math `$` second). `<name>?$: <src>` ≡ `<name>?: ${<src>}` — an optional property whose default is a math-evaluated expression. The reverse order `<name>$?:` is `E010`. Bare on a plain optional: `<name>?: <v>` (rule 17, no math). Bare on a plain math shorthand: `<name>$: <src>` (this rule, no default). A `$`-suffix value used with `?` must still be a String math source.

```yml
a:
  x$?: x + y     # E010 — wrong order (math first, optional second)
# vs
a:
  x?$: x + y     # x?: ${x + y} — math-evaluated default (optional first, math second)
```
The first `x$?:` is an error. The correct form for a math-evaluated optional default is `x?$: src`.

> This rule (rule 18) is v2. Rule 17 (`?`) is v1. Non-string `$`-suffix values and wrong modifier order surface as `E010` (invalid syntax).

### 19. Shell execution builtins (`$sh`, `$pw`)

The `$sh{<command>}` and `$pw{<command>}` forms execute a shell command and return its result as `{exit_code: Int, stdout: String}`.

**Value form:** `$sh{<command>}` / `$pw{<command>}` — the command string is interpolated (all scalar types; Array/Object → E011), then executed via the configured `CommandExecutor`. The result is always an object with `exit_code` (Int) and `stdout` (String).

```yml
content: $sh{cat path/to/file.txt}
greeting: $pw{Write-Output "Hello World!"}
```

**Property-key shorthand:** any property key ending with `$<backend>` (e.g. `name$sh:`, `$box$sh:`, `$$$t$pw:`) is sugar for wrapping the value in `$<backend>{...}`. The suffix is stripped from the key. Applies to all property keys — regular components, templates, and chained templates alike.

```yml
# Property-key shorthand (all equivalent to their value-form counterparts)
main$sh: cat path/to/file.txt
# same as
main: $sh{cat path/to/file.txt}

$box$sh: echo $name
# same as
$box: $sh{echo $name}
```

**Extensibility:** any `$<backend>{<command>}` is recognized. The backend name is passed to the executor; unknown backends → E016.

**Error handling:** non-zero exit codes are returned as part of the result (not an error). E016 is emitted only for spawn failures, missing executor (`opts.executor` is `None`), or disallowed backend (not in `allowed_backends`).

**Interpolation inside `$sh{...}`:** the command string undergoes the same interpolation as regular strings — `$name`, `$0`, `${...}` are resolved before execution. Scalar values render per *Number→string rendering*; Array/Object arguments → E011.

```yml
file: path/to/file.txt
lines: $sh{wc -l < $file}
# $file interpolates to "path/to/file.txt", then the command is executed
# lines → {"exit_code": 0, "stdout": "      42 path/to/file.txt\n"}
```

**Restriction via `_ymx.allowed_backends`:** the front-matter field `allowed_backends` (a list of strings) limits which backends may be used. If absent, all backends are allowed. If set, only the listed backends are permitted — using a non-listed backend emits E016.

```yml
_ymx:
  allowed_backends: [sh]
```
