## Project metadata

A document may carry three reserved **meta keys** at its top level — `_ymx` (front matter), `_test` (tests), and `_use` (file imports). They are not components (see [Reserved names](11-rules-string.md)); `ymx-core` strips them from the namespace and stores their raw parsed values on the `Project`, and the `ymx-config` / `ymx-test` / `ymx-lib` crates interpret them. A document with two top-level `_ymx` (or `_test` or `_use`) keys keeps the first and ignores the rest; meta keys are consumed, not registered, so E004 (duplicate name) does not apply.

### `_ymx` — front matter

`_ymx` is a mapping of compiler-flag **defaults** for the document. The recognized fields, all optional:

| Field             | Type        | Default     | Notes |
|-------------------|-------------|-------------|-------|
| `max_depth`       | int         | `256`       | recursion cap (rule 11); exceeding it raises `E008` |
| `from_keyword`    | string      | `from`      | rule 6 keyword |

| `format`          | string      | `json`      | `json` or `diagnostics` |
| `pretty`          | bool        | `false`     | pretty-print JSON output |
| `plain`           | bool        | `false`     | `true` promotes sub-namespace components **and** templates into the global namespace; `false` promotes nothing; `template` (string) promotes templates only. Invalid value → `E010`. See [Component visibility](11-rules-string.md#component-visibility) |
| `allowed_backends` | list of strings | (all) | Restricts which shell components may execute (rule 19). If absent, both `sh` and `pw` are allowed. If set (e.g. `[sh]`), only the listed backends are permitted — calling a non-listed shell component emits E016. |
| `allowed_ipc`     | list of strings | (all) | Restricts which IPC transports may be used (rule 21). If absent, all transports are allowed. If set (e.g. `[pipe, socket]`), only the listed transports are permitted — calling via a non-listed transport emits E018. |

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

> Diagnostics that are unreachable by construction — the malformed-`_test`-block case of `E010`, and YAML-parse failures (`E001`) of the document that hosts the `_test` block (an un-parseable carrier file is unreadable), plus all other load-time codes (`E004`, `E007`, `E015`) — are exercised by ordinary crate `#[test]` unit tests with inline YAML snippets (see [Testing](10-testing.md)). Every other code is reachable: option-resolution and target-compilation errors fire after `_test` is parsed, against the loaded `Project`.

### `_use` — file imports

`_use` is a mapping of explicit file imports. Its value has three forms:

- **Bare `*`**: `_use: *` — recursive wildcard. The entry file's directory is walked for all `.yml`/`.yaml` files (same filter as the legacy behaviour: skip `.git`, skip hidden directories, skip subdirectories with no YAML files directly). All public components/templates found are added to the entry's global namespace.
- **Wildcard string**: `_use: {"*": "foo"}` — import all public components and templates from `foo.yml` into the global namespace, keeping their original names.
- **Named entries**: `_use: {x: "foo.bar", ...}` — for each `alias: "file.component"`, import `component` from `file.yml` and register it under `alias` in the global namespace. The file must exist and the component must be defined (E002 if not). Multiple files may define the same component internally; renaming prevents collisions.

All imported components land in the **global namespace** of the entry file. File-scoped components (`_`-prefixed) cannot be imported (E005). If an imported file has its own `_use`, that is processed first — the imported file's own imports are resolved and registered in its namespace before it is consumed by the caller. This makes `_use`-imported names **re-exportable**: a component imported into `mid.yml` via `_use` is visible in `mid.yml`'s namespace and can be imported by other files that import from `mid.yml`. As with any component, an imported name whose effective identifier starts with `_` remains file-scoped and is **not** re-exported to files that import from the intermediate file. For example, if `mid.yml` has `_use: {middle: "leaf.deep"}`, then `middle` is available in `mid.yml`'s namespace, and an entry file can import it via `_use: {one: "mid.middle"}`. A cycle in the import graph raises `E001`.

Only the **entry file's** `_use` and those of its transitively imported files are processed. `_use` in other files (not imported) is ignored.
