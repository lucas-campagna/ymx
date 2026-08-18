# YMX

**YMX** is a YAML parser and compiler that turns a directory of `.yml`/`.yaml` files into a JSON document. It is usable as a CLI tool and as a Rust library.

YMX v1 implements rules 1–16 of the resolver (with rules 17–18 specified for forward-compatibility but not yet implemented), emitting JSON. HTML, PDF, and the web service are planned for v2+.

## Installation

### Pre-built binary

```bash
# Download from the releases page, or:
curl -fsSL https://example.com/ymx/install.sh | bash
```

### From source

```bash
# Requires Rust (latest stable; MSRV is pinned in rust-toolchain.toml)
cargo install --path crates/ymx-cli
# or
cargo install --git https://github.com/your-org/ymx  # when published

# Binary lands at ~/.cargo/bin/ymx (or target/release/ymx with --path)
ymx --version
```

### Build locally (no install)

```bash
git clone https://github.com/your-org/ymx
cd ymx
cargo build --release
./target/release/ymx --help
```

## CLI Usage

```
ymx <path> [flags]
```

`<path>` is a `.yml`/`.yaml` file or a directory containing `.yml`/`.yaml` files (a **project**). When it is a directory, the entry path defaults to `main.main` (file `main.yml`, component `main`).

### Compile a project to JSON

```bash
# Compile the project at ./my-project, output to stdout
ymx ./my-project

# Compile a single file
ymx ./my-project/main.yml

# Pretty-print JSON output
ymx ./my-project --pretty

# Write JSON to a file
ymx ./my-project --output result.json

# Use a different entry component
ymx ./my-project --entry other.main
```

### Run inline tests

```bash
# Run all _test blocks in a project
ymx --test ./my-project

# Recursively run tests in every subdirectory that contains .yml files
ymx --test .

# Run tests for a single file
ymx --test ./my-project/main.yml
```

### Common flags

| Flag | Default | Description |
|------|---------|-------------|
| `--entry <comp>` | `main.main` | Entry path as `folder.file.component` (2 segments) |
| `--pretty` | `false` | Pretty-print JSON output |
| `--max-depth <n>` | `256` | Recursion depth cap |
| `--plain` | `false` | Promote all sub-namespace names to global |
| `--plain-template` | `false` | Promote only templates from sub-namespaces |
| `--format <json\|diagnostics>` | `json` | Output format |
| `--output <file>` | (stdout) | Write output to a file |
| `--test` | (off) | Run inline `_test` blocks instead of compiling |

See `ymx --help` for the full flag list.

### Exit codes

- `0` — success (no diagnostics)
- `1` — one or more diagnostics produced (parse error, compile error, test failure)
- `2` — CLI usage error (bad flags, missing file)

## Library Use

Add `ymx-lib` to your `Cargo.toml`:

```toml
ymx-lib = { version = "0.1" }  # or { path = "crates/ymx-lib" }
```

### Minimal compile

```rust
use ymx_lib::{load_project, compile, Options};

let project = load_project("./my-project").unwrap();
let value = compile(&project, &Options::default()).unwrap();
let json = serde_json::to_string_pretty(&value).unwrap();
println!("{json}");
```

### Run tests from Rust

```rust
use ymx_lib::{load_project, Options};
use ymx_config::CliOverrides;
use ymx_test::{parse_tests, run_tests};

let project = load_project("./my-project").unwrap();
let opts = ymx_config::extract_options(&project, &CliOverrides::default_for_tests()).unwrap();
let tests = parse_tests(&project).unwrap();
let results = run_tests(&project, &opts);

for result in &results {
    if !result.passed {
        println!("FAIL: {} ({:?})", result.test.target, result.test.expected);
    }
}
```

### Access the IR directly

```rust
use ymx_lib::{load_project, compile_component, Options, Args};
use ymx_core::Value;  // also re-exported by ymx-lib

let project = load_project("./my-project").unwrap();

// Compile a specific component with arguments
let opts = Options::default();
let value = compile_component(
    &project,
    "my_component",
    &Args::Named(vec![("x".into(), Value::Int(1)), ("y".into(), Value::Int(2))]),
    &opts,
).unwrap();
```

## Project structure

```
ymx/
├── Cargo.toml
├── rust-toolchain.toml
├── crates/
│   ├── ymx-core/     # Pure compiler: parser, rules-1–16 resolver,
│   │                 #   math engine, builtins, diagnostics (no I/O)
│   ├── ymx-config/   # _ymx front-matter → Options
│   ├── ymx-test/     # _test parsing + run_tests / TestResult
│   ├── ymx-lib/      # Thin stable API: re-exports ymx-core + load_project
│   └── ymx-cli/      # Binary: arg parsing, orchestration
├── examples/
│   ├── template-demo/
│   ├── map-scalar-items/
│   └── reduce-running-total/
└── tests/cases/      # Scenario harness (tests/cases/rule-NN/<scenario>/)
```

## Examples

The `examples/` directory contains small runnable projects. Each defines `_test` blocks that assert the expected output, so you can run them directly with `--test`:

```bash
# Template demo: auto-applied $template
ymx --test examples/template-demo

# $map: apply a component to each array item
ymx --test examples/map-scalar-items

# $reduce: accumulate over an array with $last
ymx --test examples/reduce-running-total
```

## Documentation

- [Project Requirements & Design](./docs/PRD.md) — full language specification
- [Implementation plan](./docs/impl/) — versioned milestone tracking
- [Usage guide](./docs/USAGE.md) — realistic YMX snippets

## Testing

```bash
# Run all tests (crate unit tests + scenario harness)
cargo test --workspace

# Run just the scenario harness
cargo test -p ymx-test --test harness

# Run CLI integration tests
cargo test -p ymx-cli --test cli

# Lint and typecheck (must pass clean)
cargo fmt --check
cargo clippy --workspace --all-targets
```

## Status

YMX v1 implements rules 1–16 of the resolver, emitting JSON. Rules 17 (`?` optional/default-merge) and 18 (`$` math shorthand) are **specified** (see PRD.md) but not yet implemented. HTML, PDF, and web are planned for v2+.
