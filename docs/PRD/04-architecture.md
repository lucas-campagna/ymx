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
        └── rule-NN/<scenario>.yml   # one file per scenario (see Testing)
```

- **Crate boundaries**: `ymx-core` is the pure compiler — parsing, the rule-1–16 resolver, the math engine, and builtins, with no filesystem or network I/O. `ymx-config` owns the `_ymx` front-matter logic (parsing the meta key, applying the *CLI > entry-file* precedence, producing `Options`). `ymx-test` owns the `_test` meta-key logic (parsing tests, running them, `TestResult`). `ymx-lib` is intentionally small: it re-exports `ymx-core`'s public surface and adds only a thin `load_project` I/O helper that walks a directory and builds a `Project` (collecting raw `_ymx`/`_test` meta values without interpreting them). The `_ymx`/`_test` *logic* lives in `ymx-config`/`ymx-test`, never in `ymx-lib`. `ymx-cli` depends on `ymx-lib` (for `load_project` + core types), `ymx-config`, and `ymx-test`, and orchestrates `load_project` → `extract_options` → `compile` → emit (and `run_tests` under a `--test` flag). External IPC components (rule 21) use the `IpcHost`/`IpcSpec` types provided by the caller; `ymx-core` stays I/O-free.
- **YAML parsing**: `yaml-rust2` is used directly (inside `ymx-core`) so source spans (line/column) are preserved on every scalar for diagnostics.
- **Output**: YAML → intermediate `Value` IR → serialize to JSON (v1). The IR is `Null | Bool | String | Int(i64) | Float(f64) | Array | Object`; object keys preserve YAML insertion order. HTML/PDF renderers consume the same IR in later versions.
- **Math**: a `MathEngine` trait evaluates `${...}`. v1 uses dynamic operand resolution — a String-valued operand is re-scanned as a math expression when it parses as one (see rule 7), numerically coerced when possible, and `+` falls back to string concatenation otherwise. The trait is the boundary for swapping to a Lua/Python/JavaScript engine in the future.
- **Builtins**: a `Builtin` trait. v1 ships `$map`, `$reduce`, `$merge` plus the extended 1.34 wave (`$split`, `$join`, `$trim`, … `$if`, `$when`). Each builtin is a *special form* that declares its own argument-evaluation strategy (e.g. `$map`/`$reduce` keep their first argument unevaluated as a callable component). The trait is the future plugin boundary. Two further builtin **components** (`sh`, `pw` — rule 19) are ordinary namespace entries with engine-implemented (executor-backed) bodies, callable via every call form — unlike the special forms, which are paren-only.
- **Diagnostics**: structured `Diagnostic { file, line, col, component, code, message }` (where `file` is the resolved file path) rendered to stderr as `[code] file:line:col (component): message` and surfaced identically through `--format diagnostics` (one diagnostic per line, no JSON on stdout). Designed so a richer "bug report" mode (full call-stack + local-argument dump) can be added later without breaking the API. The stable error codes are listed in [Diagnostic codes](#diagnostic-codes) below.
- **Cycles**: no precise cycle detection in v1; a configurable depth cap (`--max-depth`, default 256) prevents runaway recursion and surfaces as a "max-depth exceeded" diagnostic (`E008`). On entry to each recursive operation in rule 11's pipeline — each inline `$comp(...)` call (rule 3), each `$name{...}` brace call (rule 22), each math `comp(...)` call (rule 7), each template step in a chain (rule 5), and each `from` dispatch (rule 6) — the counter is **checked before** incrementing: if `depth == max_depth` the operation aborts with `E008` (so at most `max_depth` recursive operations are allowed); otherwise `depth` is incremented by 1 and the operation proceeds.

### Diagnostic codes

Every diagnostic renders to stderr as `[code] file:line:col (component): message`. Load-time codes (`E001`, `E004`, `E007`, `E015`) are not `_test`-driveable because `load_project` is all-or-nothing; they are exercised by crate `#[test]` unit tests with inline YAML.

| Code | Stage | Diagnostic | Example |
|------|-------|------------|---------|
| `E001` | load | YAML parse error or unsupported YAML feature (multi-document stream `---`, complex mapping key, merge key `<<`); also the load-stage catch-all for I/O failures (missing root, unreadable file) and non-string top-level keys. | `a: 1\n---\nb: 2` in one file → multi-document stream not supported. `0: value` → non-string top-level key. |
| `E002` | compile | Unknown component reference — including a brace-call / key-suffix target that resolves to no component (rule 22); a builtin special-form name used in braces is E002 with a hint to use the paren form. | `a: $nonexistent` → `a` references a component that does not exist. `$typo{1}` → no component `typo` (rule 22). |
| `E003` | compile | Missing required argument. A property `$x` is referenced but neither supplied by the caller nor declared with `x?:`. | `a: $x` called with no `x` argument → missing required `x`. |
| `E004` | load | Duplicate component name in the same namespace. Two top-level definitions of `foo` in the same directory. | `a.yml` defines `x: 1` and `b.yml` also defines `x: 2` → both in the global namespace → `E004`. |
| `E005` | compile | File-scope violation. A `_`-prefixed component is referenced from outside its document. | `_helper: 42` in `a.yml`; `b.yml` calls `$a(_helper=$_helper)` → cross-document reference to file-scoped `_helper` → `E005`. |
| `E006` | compile | Ambiguous shortcut. Two properties in the same component body both match names of existing components. | `a:\n  b: 1\n  c: 2\nb: $0\nc: $0` — both `b` and `c` match components → shortcut is ambiguous → `E006`. |
| `E007` | load | Reserved name used as a component or template: any builtin special-form effective identifier (`map`, `reduce`, `merge`, `split`, `join`, `trim`, `upper`, `lower`, `replace`, `filter`, `sort`, `reverse`, `unique`, `flatten`, `first`, `last`, `slice`, `keys`, `values`, `entries`, `from_entries`, `pick`, `omit`, `type`, `is_array`, `is_object`, `is_string`, `is_number`, `is_null`, `to_string`, `to_number`, `coalesce`, `sum`, `avg`, `min`, `max`, `if`, `when`) plus the builtin components `sh`/`pw` (rule 19) — all for every leading-`$` variant. | `map: 1` → defining a component named `map` (or `$map`, `$$map`, …) is rejected. `sh: 1` → rejected (would shadow the builtin component). |
| `E008` | compile | Max-depth exceeded. The recursion depth cap (`--max-depth`, default 256) was exhausted. | A chain of 300 nested `$a()` calls hits the 256-deep limit → `E008`. |
| `E009` | options | Entry not found. Entry path has fewer than two segments, resolves to a missing file, has an ambiguous `.yml`/`.yaml` stem, or the entry name is not a valid component identifier. | `ymx .` with no `main.yml`; `ymx ./folder` where both `folder.yml` and `folder.yaml` exist; `ymx ./file.yml --entry NOT_A_VALID_NAME`. |
| `E010` | both | Invalid syntax: malformed call-site, bad string escape (`\X`), math identifier prefix `$letter`, mixed-shape template chain, unknown/invalid `_ymx` field, malformed `_test` block, malformed brace-call payload (non-identifier object key, unbalanced literal — rule 22), or wrong modifier order (`$?`, `$<name>?`). | `\n` in a string (unknown escape); `${ $x }` (bare `$` in math — drop the `$`); `plain: "maybe"` (invalid enum value); `x$?: y` (wrong modifier order); `{a-b: 1}` as a brace-call payload (non-identifier key — rule 22). |
| `E011` | compile | Math error (type mismatch, division by zero, non-numeric operand) or builtin argument type error (non-array 2nd arg to `$map`/`$reduce`; mixed-shape `$merge`). | `${true + 1}` → Bool + Int is a type mismatch. `$map(a, "not an array")` → second arg must be Array. `$merge({a:1}, [1,2])` → Object + Array is an unsupported merge shape. |
| `E012` | compile | Positional argument after a named argument in a call. | `$f(name=1, 2)` → positional arg `2` follows named `name=1` → `E012`. |
| `E013` | compile | Array/object literal as a direct call argument (paren form; the `$name{...}` brace form accepts object/array payloads — rule 22). | `$f([1,2,3])` or `$g({a:1})` → the paren form rejects inline collection literals as direct call arguments; use the brace form `$f{[1,2,3]}` (rule 22). |
| `E015` | load | Meta-key reserved name used as a component or template. A top-level key that is a leading-`$` variant of `_ymx` or `_test`. | `$_ymx:\n  v: 1` or `$$_test: 2` → leading-`$` variants of meta keys are rejected as reserved. |
| `E016` | compile | Shell execution error (executor not provided, disallowed backend, or spawn failure). | `$sh{echo hi}` with no executor → E016. `$pw{...}` when `allowed_backends: [sh]` → E016. |
| `E017` | render | Array/object children without a `from` component wrapper. | `$mycomp: {a: 1, b: 2}` with no `from` in children → E017. |
| `E018` | compile | IPC call failure: no host provided, disallowed transport, spawn failure, process crash, protocol violation, timeout, error_pattern match, or non-2xx HTTP response. Also raised when a lifecycle hook (`before_start`, etc.) fails. | `$py{print(1)}` with `Options.ipc = None` → E018. |
