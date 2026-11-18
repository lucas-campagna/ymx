## Static linting

### Purpose

`ymx-lint` is a pure, side-effect-free static analyzer for loaded YMX projects. It answers "is this project well-formed, reachable, and tidy?" without compiling side-effecting calls: it never spawns a shell, opens an IPC transport, touches the network, or mutates the project.

Lint findings use a **separate code namespace**, `L001` onward. They are **advisory by default**: an `L###` finding never changes how a component compiles, and no `L###` code is a compiler error. Compiler diagnostics (`E###`) remain hard failures of the load/option/compile/render pipeline exactly as specified in [Architecture](04-architecture.md#diagnostic-codes). Static linting introduces **no new `E###` code** and does not reinterpret any existing one.

### Crate boundary and pipeline

| Crate | Role in linting |
|-------|-----------------|
| `ymx-core` | Supplies the parsed `Project`, spans, and the call-site / string-interpolation / math / property-key parsers the linter reuses. Stays I/O-free. |
| `ymx-lib` | Remains the **only** loader: `load_project` builds the `Project`. The lint integration-test harness may call it through `ymx-lint`'s `[dev-dependencies]` entry; the `ymx-lint` library target has no normal dependency on it. |
| `ymx-config` | Defines `LintSeverity` / `FailOn` / `LintConfig` and extracts `LintConfig` from the **entry file's** `_ymx.lint` with the usual CLI > entry-file > default precedence. Unknown/invalid fields produce `E010`, as for other `_ymx` fields. |
| `ymx-lint` | **New crate**: defines `LintDiagnostic` and `LintReport`, re-exports the config types, and hosts the rule passes and the pure `lint_project(&Project, &Options, &LintConfig) -> LintReport` entry point. I/O-free; contains no `CommandExecutor` or `IpcHost`. |
| `ymx-cli` | Wires `ymx lint <path>`: resolve entry → `load_project` → `extract_options` → `extract_lint_config` → static-mode check → `lint_project` → emit. It must not install a shell executor or IPC host on this branch. |

`ymx-lint` depends one-way on `ymx-core` (types + parsers) and `ymx-config` (config types); `ymx-config` does **not** depend on `ymx-lint`, so there is no cycle. The `ymx-lint` **library target** has no normal dependency on `ymx-lib`, `ymx-test`, or `ymx-cli` and stays I/O-free. Its integration tests may list `ymx-lib` under `[dev-dependencies]` and call `load_project` to build fixtures: `ymx-lib` does not depend on `ymx-lint`, so this is not a cycle and does not affect library consumers. `ymx-cli` depends on all of them and is the only place the pipeline is orchestrated.

The config types are defined in `ymx-config` and re-exported from `ymx-lint` so callers see one lint surface:

```rust
pub enum LintSeverity { Error, Warning, Info }

pub enum FailOn { Error, Warning, Info, None }

pub struct LintConfig {
    pub deny: Vec<&'static str>,    // L-codes emitted at error severity
    pub allow: Vec<&'static str>,   // L-codes suppressed entirely
    pub fail_on: FailOn,            // Error | Warning | Info | None
}

pub struct LintDiagnostic {
    pub code: &'static str,         // L### — never E###
    pub severity: LintSeverity,
    pub file: Option<PathBuf>,
    pub line: u32,
    pub col: u32,
    pub component: Option<String>,
    pub message: String,
    pub help: Option<String>,
}

pub struct LintReport {
    pub findings: Vec<LintDiagnostic>,          // L### findings
    pub diagnostics: Vec<Diagnostic>,           // E### seen during the static check; kept separate
}
```

#### Rendering

`--format diagnostics` prints one finding per line, lint-specific and clearly distinct from compiler diagnostics:

```
[L001] warning: main.yml:3:1 (unused): component is never referenced
  help: remove the definition or add a call site
```

The optional `help` line is emitted only when present, indented by two spaces. Compiler diagnostics in the same report are prefixed `[compile E###]` and never use the `[L###]` form.

`--format json` emits a single object; L### findings and E### diagnostics live in separate arrays:

```json
{
  "findings": [
    { "code": "L001", "severity": "warning", "file": "main.yml", "line": 3, "col": 1,
      "component": "unused", "message": "component is never referenced",
      "help": "remove the definition or add a call site" }
  ],
  "diagnostics": [
    { "code": "E002", "file": "main.yml", "line": 7, "col": 5,
      "component": "main", "message": "unknown component reference" }
  ],
  "summary": { "findings": 1, "errors": 0, "warnings": 1, "info": 0 }
}
```

`summary` counts **only** L### findings. `file` is `null` when no file context exists.

### Configuration

`_ymx.lint` is a sibling field inside the entry file's `_ymx` block. It follows the [front-matter invariant](06-project-metadata.md#_ymx--front-matter): only the entry file's `_ymx` is read. `_ymx.lint` in any other document is completely ignored — never parsed or validated — even if malformed.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `deny` | list of L-codes | `[]` | Emit these rules at `error` severity, overriding their default severity. |
| `allow` | list of L-codes | `[]` | Suppress these rules entirely — no `LintDiagnostic` is produced. |
| `fail_on` | `error` \| `warning` \| `info` \| `none` | `error` | Minimum emitted severity that makes the lint command exit `1`. `none` disables failure on lint findings. |

Severity ordering is `error > warning > info`; `none` is a sentinel above all severities that disables thresholding. All six initial rules default to `warning` (advisory).

Validation, performed by `ymx-config` and reported as `E010` (a hard config error, not an L### finding):

- unknown field inside `_ymx.lint`;
- wrong value type for a field, or an unrecognized L-code (only `L001`–`L006` exist in this version);
- invalid `fail_on` value;
- the same L-code listed in both `deny` and `allow`.

**CLI precedence.** `--deny`, `--allow`, and `--fail-on` override the corresponding entry-file values (CLI > entry file > default). `--deny` and `--allow` accept comma-separated codes and may be repeated; `--fail-on` accepts one of the four values above.

### CLI

```
ymx lint <path> [flags]
```

`<path>` is resolved exactly like compile mode: an entry file whose parent directory is the project root (a directory uses `main.yml`/`main.yaml` with entry `main.main`). Lint never reads stdin.

| Flag | Meaning |
|------|---------|
| `--deny <codes>` | Override `_ymx.lint.deny`. |
| `--allow <codes>` | Override `_ymx.lint.allow`. |
| `--fail-on <level>` | Override `_ymx.lint.fail_on`. |
| `--format <diagnostics\|json>` | Output style; default `diagnostics`. Lint accepts only these two values. |
| `--entry`, `--plain`, `--plain-template`, `--max-depth`, `--allowed-backends`, `--allowed-ipc` | Same meaning as compile mode; they shape the static check only. |

`--no-exec` and `--no-ipc` are implied by lint mode. Compile-only flags (`-c`, `--test`, `--watch`, `--output`, `--pretty`) are usage errors under `ymx lint`.

**Exit codes.**

- `0` — no emitted L### finding at or above `fail_on`. Compiler E### diagnostics may still be printed.
- `1` — at least one emitted L### finding at or above `fail_on`, **or** a load/option failure that prevents analysis at all (e.g. `E001`, `E004`, `E007`, `E009`, `E010`, `E015`).
- `2` — usage error (missing path, unknown/incompatible flag, bad flag value, unsupported format).

**Compiler diagnostics.** The static check may surface E### diagnostics such as `E002` (unknown reference), `E003` (missing argument), or `E016`/`E018` (disabled execution). They are returned in `LintReport::diagnostics` and printed alongside findings, but they are **not** L### findings: they are excluded from `summary` and do not affect the lint exit code. This keeps lint advisory and prevents pre-existing hard errors from silently becoming lint failures. Load/option failures abort before analysis and exit `1` as above.

### Safety: static execution mode

1. `ymx-cli` must not construct or inject `StdExecutor` or `StdIpcHost` on the lint branch.
2. `lint_project` must force `opts.executor = None` and `opts.ipc = None` (or an equivalent static mode) on a **local copy** of `Options` before any compile check, regardless of what the caller supplied.
3. Shell and IPC constructs are still analyzed: `$sh`/`$pw` call sites, shell payloads, IPC alias declarations, and IPC alias calls all contribute references to the call graph and to L005/L006.
4. E### diagnostics produced **solely** because execution is disabled (`E016`/`E018`) are captured as compiler diagnostics with a hint that lint mode is static; they may be suppressed or downgraded to `info`-severity lint hints. They are never executed, never retried, and never promoted to hard lint failures.
5. `ymx-lint` imports no filesystem, process, or network APIs. It is I/O-free by construction.

### Call-graph and reference extraction

Nodes are declaration sites in the loaded `Project`: components, templates (all leading-`$` chain depths), file-scoped names, builtin components, and IPC aliases registered from `_use`. Every finding points at the declaration or reference site that caused it, using spans from the parsed `Project`.

| Source | Syntax | Recorded edge |
|--------|--------|---------------|
| Inline call | `$name(...)` (rule 3) | component call `site → name`; supplied args → target slots |
| Brace call / key suffix | `$name{...}`, `key$name:` (rule 22) | component call; payload fields → target slots |
| `from` dispatch | `from: target` (rule 6) | component dispatch `site → target`; all slots forwarded |
| Shortcut properties | property name matching a component (rule 8) | component call `site → matched component` |
| Math calls | `${... name(...) ...}` (rule 7) | component call `site → dotted target` |
| Interpolation | `$name`, `$N` (rules 2, 4, 7) | **argument-slot read**, not a component call |
| Shell payloads | `$sh{...}`, `$pw{...}`, `key$sh:` (rule 19) | call to builtin `sh`/`pw`; payload scanned for interpolations (L005) |
| IPC alias calls | any call form targeting a `_use` alias (rule 21) | component call `site → alias` |

All reference sites must be collected by reusing `ymx-core`'s existing parsers — the call-site grammar, the string-interpolation scanner, the math parser, `ParsedKey` for property-key modifiers, and the `from` value parser. **`ymx-lint` must not introduce a second YAML parser or a regex scanner over raw YAML text.** If a core parser needs a read-only accessor to be reusable, that accessor is a core API addition, not a new parser in the linter.

### Rules

All rules default to `warning`, are suppressible with `allow`, and can be promoted with `deny`.

#### L001 — unused component or template

- **Reports:** the declaration site of a user-defined component or template with zero incoming references.
- **Definition:** In the whole-project reference graph, a component is *used* if it is the entry component, a `_test` target, a hook target (`on_request` / `on_response` / `on_error`), or has at least one incoming component-call edge — including an edge from an otherwise unused component. A template at chain depth *k* is used iff the name at depth *k−1* with the same effective identifier is used and the chain does not stop at a missing predecessor (rule 5). File-scoped names match only inside their own document. Builtin components and special forms are never reported. IPC aliases declared in `_use` are excluded from L001 and covered only by L006, so the same alias never produces both findings.
- **Help:** "remove the definition, or add a call site."

#### L002 — unreachable component from entry

- **Reports:** a component with no path from the root set.
- **Definition:** Roots are the entry component plus `_test` targets and hook targets. Traverse component-call edges, adding each matched template as an implicit edge from its target (rule 5). A component reachable only from unreachable components is reported; cycles attached to a root are all reachable, cycles disconnected from every root are all reported. Builtins are excluded. Templates are not reported by this rule (L001 covers them).
- **Relation to L001:** L001 means *zero incoming references*; L002 means *no path from the entry*. A component called only by another unreachable component produces L002 but not L001.
- **Help:** "wire this component into a `from`, `$name(...)`, `$name{...}`, shortcut, or math call reachable from the entry."

#### L003 — unused argument

- **Reports:** a property key (slot) in a component body that is never read in that body.
- **Definition:** Strip key modifiers (rules 17, 18, 22) to obtain the slot name; integer keys denote positional slots `$N`. A slot is *read* if it occurs as `$name` / `$N` interpolation (including inside a larger string), as a math identifier inside `${...}`, or through a template implicitly applied to the component. A component with a `from` target forwards all slots, so it yields no L003 findings. A `?` default declaration is a declaration, not a read. Builtin component bodies are skipped. Emit at the property key's span.
- **Help:** "remove the unused property, or reference it with `$name`."

#### L004 — caller passes an argument the target never reads

- **Reports:** a supplied argument whose slot is unread by the resolved target.
- **Definition:** For every component-call edge carrying arguments, apply the L003 read analysis to the resolved target and emit at each unread argument's span. Skip unresolved targets (E002), builtin special forms, and `from`-forwarding targets (they forward every argument). `$name{...}` payload fields follow rule 22's positional/named argument mapping and are checked identically.
- **Help:** "drop the unused argument `name=value`."

#### L005 — shell call with interpolated payload

- **Reports:** a shell invocation whose payload contains interpolation.
- **Definition:** Sites are `$sh{...}`, `$pw{...}`, `key$sh:` / `key$pw:`, `from: sh` / `from: pw` with interpolated properties, and any call resolving to the `sh`/`pw` builtin components. A payload is *interpolated* when its parsed string contains an unescaped `$name`, `$N`, `${...}`, `$name(...)`, or `$name{...}`; escaped `\$` does not count. Emit at the payload span. Nothing is executed.
- **Help:** "pass data as separate arguments, or escape/sanitize the interpolated value before it reaches the shell."

#### L006 — IPC alias declared but never called

- **Reports:** an IPC alias from `_use` (including transitively re-exported aliases) with no call sites.
- **Definition:** An alias is *called* if any reference site targets it: `$alias(...)`, `$alias{...}`, `key$alias:`, `from: alias`, math `alias(...)`, or a hook field naming it. Emit at the `_use` entry's key span; re-exported aliases are reported at the declaring file's key span. File imports (string RHS of `_use`) are not IPC aliases and are not reported here.
- **Help:** "remove the `_use` entry, or add a call site."

### Tests

Lint fixtures live in dedicated directories under `tests/lint/rules/<rule>/<case>/`, each a real YMX project root. They are **never** placed under `tests/cases/` and **never** use the `_test` meta key for lint assertions.

A Rust integration-test harness in `ymx-lint` loads each fixture with `ymx_lib::load_project` — `ymx-lib` is declared as a `[dev-dependencies]` entry only — builds `Options` and `LintConfig`, calls `lint_project`, and asserts the exact `(code, severity, file, line, col, component)` tuples. Expected findings are encoded inline in Rust or in a small fixture-local file (e.g. `expected.yml` / `expected.json`) next to the fixture. The harness may perform file I/O to load fixtures; this does not make the `ymx-lint` library target I/O-capable or stateful, and introduces no dependency cycle because `ymx-lib` does not depend on `ymx-lint`.

Coverage must include at least one positive and one negative case per rule, `deny` / `allow` / `fail_on` precedence, non-entry `_ymx.lint` being ignored, static-mode suppression of `E016`/`E018`, and both output renderings.
