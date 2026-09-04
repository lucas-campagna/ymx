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

  > Escape expansion (`\n`, `\t`, `\\`) is applied to stdin content as a YAML fallback: if the raw content fails to parse as YAML, the expanded version is tried. JSON is attempted first (raw) and already handles these escapes natively.

-`--test` is unaffected by stdin modes**; it requires a `path` argument and does not read stdin in either case.

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
- `--errors`: equivalent to `--format diagnostics` but can be combined with other flags. When set, only errors are printed to stderr; no JSON on stdout.
- `--no-exec`: disable shell execution entirely (sets `opts.executor = None`). Calling `$sh{...}` or `$pw{...}` then emits E016.
- `--allowed-backends <list>`: comma-separated list of allowed shell backends (overrides `_ymx.allowed_backends`). Calling a non-listed backend emits E016.
- `--no-ipc`: disable IPC entirely (sets `opts.ipc = None`). Calling an IPC component then emits E018.
- `--allowed-ipc <list>`: comma-separated list of allowed IPC transports (overrides `_ymx.allowed_ipc`). Using a non-listed transport emits E018.
- `--test`: run inline `_test` cases (via `ymx-test`) instead of compiling the entry. When `path` is omitted it defaults to `.` (the current directory). When `path` is a **directory** (or `.`), recursively search for `.yml`/`.yaml` files up to depth 10; each file is its own independent project (its parent directory is the project root). If the max search depth is reached while walking, emit `warning: max search depth (10) exceeded in <path>; skipping <folder>` and continue. Files and directories matching `.gitignore` patterns are skipped. Aggregate results across all projects; exit non-zero if any test fails. When `path` is a **file**, run tests for that single project (the existing behaviour). `--test` does not read stdin.
- `-c, --code <yml>`: inline YAML or JSON component definitions. Accepts both YAML and JSON formats (auto-detected, same as stdin args). Four usage modes:
  - **No file, `-c` only**: the inline content is the script; entry is `main.main` (or `--entry` override). Equivalent to `cat script.yml | ymx` but without a file on disk.
  - **File + `-c`**: load the file normally (entry derived from file stem), then overlay the `-c` components onto the global namespace. Components from `-c` **completely replace** any file component with the same name (last-registered-wins). New names from `-c` are added. The file's `_ymx` / `_test` metadata is unaffected.
  - **Stdin args + `-c`** (no file): the inline content is the script; stdin provides call arguments (JSON/YAML, same as the stdin-args mode).
  - **File + stdin args + `-c`**: load the file, overlay `-c`, stdin provides call arguments.
  `-c` does **not** contribute `_ymx` or `_test` metadata — only the file's front matter applies. `-c` combined with `--test` is a CLI usage error (exit 2).

**Exit codes.** `0` on success; `2` for CLI usage errors (missing required argument, `--stdin` with a terminal, etc.); `1` for any diagnostic produced by the pipeline (parse/namespace errors, E001/E004/…, a missing entry file, a missing entry component, max-depth, or a failing `_test` under `--test`). With `--format diagnostics` on a successful compile, stdout is empty and the exit code is `0`.

**Orchestration.** The CLI is the canonical full pipeline: `ymx_lib::load_project(path.parent())` → `ymx_config::extract_options(&project, &cli)` → `ymx_core::compile(&project, &opts)` (or `ymx_test::run_tests(&project, &opts)` under `--test`) → serialize/emit. When stdin provides call arguments (no `path` or `path` given but stdin provides args), `compile` is replaced by `ymx_core::compile_component(project, "<entry>", &args, &opts)`. The project root is derived from the file's parent directory; `--entry` is resolved before `extract_options` because it selects the front-matter source file (see [`_ymx` — front matter](06-project-metadata.md#_ymx--front-matter)). When `--test` is given with a directory path (or `.` by default), the CLI searches that directory recursively (up to depth 10, skipping `.gitignore` patterns), treating each `.yml`/`.yaml` file as an independent project root, and runs `load_project` → `extract_options` → `parse_tests` + `run_tests` per project, aggregating results and exiting non-zero if any test fails across all projects.
