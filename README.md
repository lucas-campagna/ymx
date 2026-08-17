# YMX

**YMX** is a YAML parser and compiler for documents, HTML, and PDF — usable as a CLI tool and a library.

YMX lets you describe rich, reusable, composable documents that compile to JSON (v1). It provides:

- **Components**: top-level YAML keys that accept arguments and return values
- **Templates**: auto-applied components prefixed with `$`
- **Namespaces**: subdirectories form sub-namespaces accessed via dotted paths
- **Math expressions**: `${...}` syntax for calculations
- **Builtin operations**: `$map`, `$reduce`, `$merge` for array/object manipulation
- **Inline tests**: `_test` blocks for assertion-driven development

## Installation

```bash
cargo install ymx
```

Or build from source:

```bash
cargo build --release
# binary at target/release/ymx
```

## CLI Usage

```bash
ymx <path> [flags]
```

Compile a YMX project (a directory of `.yml`/`.yaml` files) to JSON:

```bash
ymx ./my-project
```

Run inline tests:

```bash
ymx --test ./my-project
```

### Common Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--entry <path>` | `main.main` | Entry path as `folder.file.component` |
| `--pretty` | `false` | Pretty-print JSON output |
| `--max-depth <n>` | `256` | Recursion depth limit |
| `--plain` | `false` | Promote all sub-namespace names to global |
| `--plain-template` | `false` | Promote only templates from sub-namespaces |
| `--format <json\|diagnostics>` | `json` | Output format |

See `ymx --help` for the full flag list.

## Library Use

```rust
use ymx_lib::{load_project, compile, Options};

// Load a project
let project = load_project("./my-project").unwrap();

// Compile the default entry (main.main) to JSON
let value = compile(&project, &Options::default()).unwrap();
println!("{}", serde_json::to_string_pretty(&value).unwrap());
```

See `ymx-lib` for the full API.

## Documentation

- [Project Requirements & Design](./docs/PRD.md) — full language specification
- [Implementation Plan](./docs/impl/) — versioned milestone tracking

## Project Structure

```
ymx/
├── crates/
│   ├── ymx-core/     # Parser, resolver, math engine, builtins (no I/O)
│   ├── ymx-config/   # _ymx front-matter extraction
│   ├── ymx-test/     # _test parsing and test runner
│   ├── ymx-lib/      # Thin API façade + load_project helper
│   └── ymx-cli/      # Binary: CLI orchestration
├── tests/cases/      # Integration scenarios (harness-driven)
└── examples/         # Small demo projects
```

## Testing

```bash
# Run all tests (crate unit tests + scenario harness)
cargo test --workspace

# Run just the scenario harness
cargo test -p ymx-test --test harness

# Run CLI integration tests
cargo test -p ymx-cli --test cli
```

## Examples

```bash
# Template demo: components + auto-applied templates
ymx --test examples/template-demo

# Map demo: $map applies a component to each array item
ymx --test examples/map-scalar-items

# Reduce demo: $reduce accumulates over an array
ymx --test examples/reduce-running-total
```

## Status

YMX v1 implements rules 1–16 of the resolver, emitting JSON. HTML, PDF, and web service are planned for v2+.
