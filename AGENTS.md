# AGENTS.md

Guidance for AI agents working on YMX. Read `docs/PRD.md` for the full specification and `docs/impl/` for the versioned implementation plan.

## What this project is

YMX is a YAML parser and compiler for documents, HTML, and PDF — usable as a CLI, a web service, and a library. **v1** implements the rules-1–16 resolver emitting JSON; CLI + library only (no HTML/PDF/web). Written in Rust (edition 2021).

## Build, test, lint

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets
```

Run these before declaring any task done. Lint and typecheck (`cargo clippy`) must pass clean. The MSRV is pinned in `rust-toolchain.toml` (latest stable).

## Agents (primary commands)

The workflow is **plan → update → build**, driven by three primary agents. All structural info is read from `docs/`; milestone `status: done` flips happen only after `gatekeeper` passes.

- **`/ymx-plan`** — default chat/planning agent. Read-only discussion: reads `docs/`, proposes milestones and spec edits, resolves ambiguities in conversation. Cannot edit files or spawn subagents; hands off to `/ymx-update`.
- **`/ymx-update`** — docs & scenario editor. Spawns `spec-curator` (`docs/PRD.md` + `docs/impl/*`) and `scenario-author` (`tests/cases/`). Flips milestone `status: done` after `/ymx-build`'s gatekeeper passes. Can edit `docs/**` only.
- **`/ymx-build`** — implementation orchestrator. Spawns the code specialist for the target crate, runs the `gatekeeper` verifier, and on PASS commits `crates/`+root manifests and creates the annotated tag `v<version>`. Never edits docs or crate code itself.

## Crate boundaries (do not violate)

Workspace at `crates/*`; see `docs/PRD.md` §Architecture.

- **`ymx-core`** — pure compiler: parser, rules-1–16 resolver, math engine, builtins, diagnostics. **No I/O.** No filesystem, no network.
- **`ymx-config`** — `_ymx` front-matter → `Options`. Owns `extract_options` and the CLI > entry-file > default precedence.
- **`ymx-test`** — `_test` meta-key logic: `parse_tests`, `run_tests`, `TestResult`.
- **`ymx-lib`** — thin façade: re-exports `ymx-core` public surface + `load_project` I/O helper. **No `_ymx`/`_test` logic** lives here.
- **`ymx-cli`** — binary: arg parsing, orchestration (`load_project` → `extract_options` → `compile`/`run_tests` → emit).
- **`ymx-web`** — stub crate (v3+).

`ymx-lib` does **not** depend on `ymx-config` or `ymx-test`. `ymx-cli` depends on all three and orchestrates the pipeline. YAML parsing uses `yaml-rust2` **directly inside `ymx-core`** so source spans (line/column) are preserved on every scalar for diagnostics. JSON output uses `serde_json` with the `preserve_order` feature (backed by `indexmap`) so object keys keep YAML insertion order.

## Architecture invariants (easy to get wrong)

1. **Entry path ≠ namespace dotted path.** The entry is a file-path address `<folder.path>.<file>.<component>` (default `main.main` = root folder + `main.yml` + component `main`; `a.b.c` = folder `a` / `b.yml` / comp `c`). The penultimate segment is a file stem; both `.yml` and `.yaml` existing is `E009`. One segment is `E009`. `from: subdir.comp` and math `subdir.comp(...)` use the **namespace** dotted path — a different model. Do not conflate them.

2. **`load_project` is all-or-nothing.** Any load-time diagnostic → `Err(Vec<Diagnostic>)`, no `Project` produced. Therefore load-time codes (`E001`, `E004`, `E007`, `E015`) are **not** `_test`-driveable — `run_tests` needs an already-loaded `Project`. Load-time codes are exercised by crate `#[test]` unit tests with inline YAML, not by scenario `_test` blocks.

3. **Templates are namespaced by default** (a `$box` in `subdir/` applies only to `subdir` components). `_ymx.plain` (`false`|`true`|`template`) and CLI `--plain`/`--plain-template` promote sub-namespace names into global. `--plain` and `--plain-template` are mutually exclusive. A promoted name colliding with an existing global name is `E004`. Template-chain lookup consults the component's own namespace first, then global per `plain`; a broken link stops the chain (no skip from `$a` to `$$a`).

4. **Non-entry `_ymx` blocks are completely ignored** — never parsed or validated. Only the entry file's `_ymx` supplies front matter. Unknown/invalid `_ymx` fields in the entry file → `E010`.

5. **`Diagnostic` carries its resolved file path** (`file: Option<PathBuf>`) so load-errors render without a `Project` to resolve against. Render format: `[code] file:line:col (component): message`.

6. **Depth cap semantics.** On entry to each recursive op (inline `$comp()` call, math `comp()` call, bare-`$name` component fallback, template step, `from` dispatch), check `depth == max_depth` → `E008` (abort); else `depth += 1` and proceed. Exactly `max_depth` recursive ops are allowed (default 256).

7. **Single shared f64 renderer** for JSON output AND string interpolation. Integer-valued floats keep the fractional part (`2.0` → `"2.0"`). Rust's default `{}` formatting is **not** used (it drops the fractional part). See `ir::render_f64`.

8. **`compile_component` takes a namespace-qualified component** (bare = global/`plain`-promoted; dotted `subdir.comp` = sub-namespace). `compile` resolves `opts.entry` (entry path) to the qualified component + no args. `ymx-test::run_tests` uses `compile_component` per test target.

## v1 scope (do not build v2 yet)

Rules 1–16 only. Rules 17 (`?` optional/default-merge) and 18 (`$` math shorthand) are **v2** — they are specified so v1 stays forward-compatible but must **not** be implemented in v1. Treat `?`/`$` property-key modifiers as a parse error (`E010`) in v1 unless told otherwise. No HTML, PDF, or web in v1.

## Builtins

`$map`, `$reduce`, `$merge` are **special forms** — each declares its own argument-evaluation strategy. `$map`/`$reduce` keep the first arg unevaluated (callable) and evaluate the array arg eagerly; `$merge` evaluates both eagerly. Effective identifiers `map`/`reduce`/`merge` are reserved (`E007`); builtins are invoked only via their `$`-prefixed forms. `$reduce([])` → `Value::Null`. `$map`/`$reduce` scalar (non-object) array items bind `$0`.

## Testing

Tests are first-class: scenarios live in `tests/cases/rule-NN/<scenario>/` as real YMX projects with `_test` blocks (no hand-written expected-output files). A scenario must define ≥1 `_test` entry. The harness: `load_project` → `extract_options(default_for_tests)` → `run_tests` → assert every test `passed`. Only post-load codes are `_test`-assertable (see invariant #2). Details: `docs/PRD.md` §Testing.

## Code style

- Follow existing crate conventions; mimic neighboring code.
- No comments unless explicitly requested.
- Rust edition 2021; `serde_json` + `preserve_order`; `yaml-rust2` for YAML.
- `ymx-core` must stay I/O-free.

## Key references

- `docs/PRD.md` — full spec (rules, diagnostic codes, library API, CLI).
- `docs/impl/README.md` — implementation plan index (milestones 1.1–1.11).
- `docs/impl/<version>-*.md` — per-milestone tasks/subtasks; update status (`planned` → `in-progress` → `done`) as work lands.

## Diagnostic codes (quick reference)

| Code  | Stage | Diagnostic |
|-------|-------|------------|
| `E001` | load | YAML parse error / unsupported YAML feature |
| `E002` | compile | Unknown component reference |
| `E003` | compile | Missing required argument |
| `E004` | load | Duplicate component name in same namespace |
| `E005` | compile | File-scope violation (cross-document `_` reference) |
| `E006` | compile | Ambiguous shortcut |
| `E007` | load | Reserved builtin name (`map`/`reduce`/`merge`) |
| `E008` | compile | Max-depth exceeded |
| `E009` | options | Entry not found (malformed path / missing file / missing component / ambiguous stem) |
| `E010` | both | Invalid syntax (call-site, escape, math id, mixed-shape chain, unknown/invalid `_ymx` field, malformed `_test`) |
| `E011` | compile | Math error / builtin argument type error |
| `E012` | compile | Positional arg after named arg |
| `E013` | compile | Array/object literal as direct call arg |
| `E015` | load | Meta-key reserved name (`$_ymx`, `$$test`, …) |

E014 is intentionally absent (E003 covers it). Load codes are not `_test`-driveable; options/compile codes are.