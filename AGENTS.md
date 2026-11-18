# AGENTS.md

Guidance for AI agents working on YMX. The full specification is modular under `docs/PRD/` (start at `docs/PRD/README.md`); the implementation plan is `docs/impl/README.md` plus `docs/impl/<version>-*.md`.

Older README text and some agent prompts still describe the original v1-only project (rules 1–16, JSON only). Treat those as historical; prefer the current `docs/PRD/`, `docs/impl/`, and code when they conflict.

## What this project is

YMX is a Rust (edition 2021) YAML parser and compiler. A project is a directory of `.yml`/`.yaml` files; each top-level key defines a reusable component, and its properties are the arguments callers can pass. Components can call each other, apply template chains, interpolate strings, evaluate math, and dispatch through `from`.

The current tree implements resolver rules 1–22, including `?`/`$` property-key modifiers (rules 17–18), shell components `sh`/`pw` (rule 19), the HTML renderer (rule 20), IPC-backed external components via `_use` (rule 21), and `$name{...}` brace calls (rule 22). Output is JSON by default; `compact`, `diagnostics`, `html`, and `pdf` formats also exist. CLI and Rust library are the supported surfaces; `ymx-web` is a WASM/TypeScript library under development.

## Build, test, lint

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets
```

Run these before declaring any task done. Lint and typecheck (`cargo clippy`) must pass clean. The MSRV is pinned in `rust-toolchain.toml` (latest stable).

## Agents (primary commands)

The workflow is **plan → update → build**, driven by three primary agents. All structural info is read from `docs/`; milestone status and version tags are owned by `/ymx-build` after verification.

- **`/ymx-plan`** — default chat/planning agent. Read-only discussion: reads `docs/`, proposes milestones and spec edits, resolves ambiguities in conversation. Cannot edit files or spawn subagents; hands off to `/ymx-update`.
- **`/ymx-update`** — docs and scenario editor. Spawns `spec-curator` (`docs/PRD/` + `docs/impl/*`) and `scenario-author` (`tests/cases/`); may edit `docs/**` directly, but delegates scenario files. Does not flip milestone status.
- **`/ymx-build`** — implementation orchestrator. Spawns the code specialist for the target crate plus the `gatekeeper` verifier. Never edits crate code itself: all implementation happens in specialists. After `gatekeeper` passes, it toggles checkboxes/status in `docs/impl/`, commits, and creates the annotated tag `v<version>`. Never force-move or delete a version tag.
- **`gatekeeper`** — read-only verifier: `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`, and crate-boundary checks. No milestone is done before it returns `GATEKEEPER: PASS`.

## Crate boundaries (do not violate)

Workspace at `crates/*`; see `docs/PRD/04-architecture.md`.

- **`ymx-core`** — pure compiler: spanned YAML parser, IR, resolver (rules 1–22), math engine, interpolation, builtins, HTML renderer, diagnostics, and IPC/executor traits and types. **No I/O**: no filesystem, network, process spawning, or environment access. Shell and IPC work is injected via `CommandExecutor` / `IpcHost`.
- **`ymx-config`** — `_ymx` front matter → `Options`. Owns CLI > entry-file > default precedence, entry resolution, plain-mode promotion checks, and option-stage diagnostics. I/O-free.
- **`ymx-test`** — `_test` meta-key logic: `parse_tests`, `run_tests`, `TestResult`, and `Expected::{Value, Error, Html}`. I/O-free.
- **`ymx-lib`** — thin façade: re-exports `ymx-core`'s public surface and owns `load_project` (filesystem walking, `_use` graph resolution, namespace merge, file-scope handling, raw meta collection). It also supplies concrete I/O implementations such as `StdExecutor` and `StdIpcHost`. No `_ymx`/`_test` interpretation lives here.
- **`ymx-cli`** — binary: arg parsing (stdin modes, `-c`, `--test`, `--errors`, exec/IPC flags), orchestration (`load_project` → `extract_options` → `compile`/`run_tests` → emit), and optional PDF/watch/HTTP features.
- **`ymx-web`** — WASM/TypeScript library crate (`wasm-bindgen`, `Ymx` class, JS math executor). Milestone 2.1 is still listed as planned in `docs/impl/README.md`.

`ymx-lib` does **not** depend on `ymx-config` or `ymx-test`. `ymx-cli` depends on all three and orchestrates the pipeline. YAML parsing uses `yaml-rust2` **directly inside `ymx-core`** so source spans (line/column) are preserved on every scalar for diagnostics. JSON output uses `serde_json` with the `preserve_order` feature so object keys keep YAML insertion order.

## Architecture invariants (easy to get wrong)

1. **Entry path ≠ namespace dotted path.** The library entry path is a file-path address `<folder.path>.<file>.<component>`: the penultimate segment is the file stem, earlier segments are folders, and the last segment is the component (default `main.main`). The CLI derives a 2-segment form `<file_stem>.<component>` from its positional file path plus `--entry`. Both `.yml` and `.yaml` existing for a stem is `E009`; fewer than two segments is also `E009`. `from: subdir.comp`, `$name(...)`, and math `subdir.comp(...)` use the **namespace** dotted path — a different model. The CLI resolves the entry before `extract_options` because it selects the front-matter source file.

2. **`load_project` is all-or-nothing.** Any load-time diagnostic → `Err(Vec<Diagnostic>)`, no `Project` produced. Ordinary `_test` `error` assertions therefore target post-load codes only. `_test._build_error: <code>` is the escape hatch for asserting load/option-time failures (e.g. `E009`, invalid `_ymx` fields) outside `run_tests`. Other load-time codes are exercised by crate `#[test]` unit tests with inline YAML.

3. **Templates are namespaced by default** (a `$box` in `subdir/` applies only to `subdir` components). `_ymx.plain` (`false`|`true`|`template`; CLI `--plain`/`--plain-template`) promotes sub-namespace names into global. `--plain` and `--plain-template` are mutually exclusive. A promoted name colliding with an existing global name is `E004`. Template-chain lookup consults the component's own namespace first, then global per `plain`; a broken link stops the chain (no skip from `$a` to `$$a`).

4. **Non-entry `_ymx` blocks are completely ignored** — never parsed or validated. Only the entry file's `_ymx` supplies front matter. Unknown/invalid `_ymx` fields in the entry file → `E010`. `entry` is not a valid `_ymx` field.

5. **`Diagnostic` carries its resolved file path** (`file: Option<PathBuf>`) so load errors render without a `Project` to resolve against. Render format: `[code] file:line:col (component): message`.

6. **Depth cap semantics.** On entry to each recursive op — inline `$comp(...)` call, `$name{...}` brace call, math `comp(...)` call, template step, or `from` dispatch — check `depth == max_depth` → `E008` (abort); else `depth += 1` and proceed. Exactly `max_depth` recursive ops are allowed (default 256).

7. **Single shared f64 renderer** for JSON output and string interpolation. Integer-valued floats keep the fractional part (`2.0` → `"2.0"`). Rust's default `{}` formatting is **not** used. See `ymx_core::ir::render_f64`.

8. **`compile_component` takes a namespace-qualified component** (bare = global/`plain`-promoted; dotted `subdir.comp` = sub-namespace). `compile` resolves `opts.entry` to the qualified component with no args. `ymx-test::run_tests` uses `compile_component` per test target.

9. **Property-key modifiers are implemented, not errors.** `?` is optional/default-merge (rule 17), `$` is math shorthand (rule 18), and `?$` combines them. Wrong modifier order (e.g. `$?`) is `E010`.

10. **I/O is injected into core.** Shell calls use `Options.executor: Option<Arc<dyn CommandExecutor>>`; with `None` (CLI `--no-exec`) they fail with `E016`. IPC calls use `Options.ipc: Option<Arc<dyn IpcHost>>`; with `None` (CLI `--no-ipc`) they fail with `E018`. `allowed_backends` / `allowed_ipc` further restrict what may run.

11. **Call forms differ.** `$name(...)` paren calls reject object/array literals as direct args (`E013`); `$name{...}` brace calls accept structured payloads (rule 22). Special-form builtins are `$`-prefixed and paren-only; builtin components `sh`/`pw` are callable through every form (`from`, shortcut, parens, braces, math).

12. **Output is a second pass over the IR.** `Format::{Json, Compact, Diagnostics, Html, Pdf}` selects emission. HTML rendering uses `from`/`children` and literal `<tag>` keys; array/object children without a `from` wrapper are `E017`. PDF is emitted from rendered HTML and requires a CLI PDF feature (`pdf-system` or `pdf-bundled`).

## Builtins

`$map`, `$reduce`, `$merge`, and the 1.34-wave builtins (`$split`, `$join`, `$trim`, `$upper`, `$if`, `$when`, …) are **special forms**: each declares its own argument-evaluation strategy. `$map`/`$reduce` keep the first arg unevaluated (callable) and evaluate the array arg eagerly; `$merge` evaluates both eagerly. Their effective identifiers are reserved (`E007`); they are invoked only via their `$`-prefixed paren forms. `$reduce([])` → `Value::Null`, and `$map`/`$reduce` scalar (non-object) array items bind `$0`.

`sh` and `pw` are different: they are builtin **components**, not special forms. They have engine-backed bodies returning `{exit_code, stdout, stderr}` and are reserved names (`E007`) for every leading-`$` variant.

## Testing

Tests are first-class. Most scenarios live in `tests/cases/rule-NN/<scenario>.yml` as real YMX projects (one file = one project root); multi-file, `_use`, and IPC cases use subdirectories under `tests/cases/` (e.g. `rule-21/`, `use/transitive/`). Assertions are written in `_test` blocks — no hand-written expected-output files. A scenario must define ≥1 `_test` entry.

Harness: `load_project` → `extract_options(default_for_tests)` → `parse_tests` + `run_tests` → assert every result `passed`. `_test` supports `Expected::Value`, `Expected::Error` (post-load codes), `Expected::Html`, and `_build_error` (load/option-time codes). Only the malformed-`_test` part of `E010` and YAML parse failures in the host document are untestable via `_test`; use crate unit tests for those. Details: `docs/PRD/10-testing.md`.

## Code style

- Follow existing crate conventions; mimic neighboring code.
- No comments unless explicitly requested.
- Rust edition 2021; `serde_json` + `preserve_order`; `yaml-rust2` for YAML.
- `ymx-core` must stay I/O-free.

## Key references

- `docs/PRD/README.md` — index to the full modular spec.
- `docs/PRD/04-architecture.md` — crate boundaries and the canonical diagnostic-code table.
- `docs/PRD/11-rules-string.md` – `docs/PRD/15-rules-19-22.md` — the language rules.
- `docs/impl/README.md` — implementation plan index and milestone status.
- `.opencode/agent/*.md` — primary commands and specialist subagent definitions.

## Diagnostic codes (quick reference)

| Code  | Stage | Diagnostic |
|-------|-------|------------|
| `E001` | load | YAML parse error / unsupported YAML feature |
| `E002` | compile | Unknown component reference |
| `E003` | compile | Missing required argument |
| `E004` | load | Duplicate component name in same namespace |
| `E005` | compile | File-scope violation (cross-document `_` reference) |
| `E006` | compile | Ambiguous shortcut |
| `E007` | load | Reserved builtin/special-form or `sh`/`pw` name |
| `E008` | compile | Max-depth exceeded |
| `E009` | options | Entry not found (malformed path / missing file / ambiguous stem / invalid component name) |
| `E010` | both | Invalid syntax (call-site, escape, math id, modifier order, mixed-shape chain, `_ymx`, `_test`, brace payload) |
| `E011` | compile | Math error / builtin argument type error |
| `E012` | compile | Positional arg after named arg |
| `E013` | compile | Array/object literal as direct paren-call arg |
| `E015` | load | Meta-key reserved name (`$_ymx`, `$$test`, …) |
| `E016` | compile | Shell execution error (no executor / disallowed backend / spawn failure) |
| `E017` | render | Array/object children without a `from` wrapper |
| `E018` | compile | IPC failure (no host / disallowed transport / spawn / protocol / timeout / hook) |

E014 is intentionally absent (E003 covers it). Load-time codes are not directly `_test`-driveable; option/compile/render codes are (with `_build_error` covering load/option-time failures). The CLI `--errors` quick table in `crates/ymx-cli/src/errors.rs` may lag behind this list — `docs/PRD/04-architecture.md` and `ymx-core::diag` are authoritative.
