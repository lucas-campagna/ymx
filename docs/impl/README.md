---
version: "1.x"
title: "YMX v1 implementation plan"
short: "Index of versioned implementation milestones (1.1 – 1.14)"
description: |
  Tracks the build of YMX v1 per docs/PRD.md: the rules-1–16 resolver, JSON
  output, CLI + library, inline `_test`, file-path entry addressing, and
  namespace promotion via `_ymx.plain`. Each milestone is a separate file with
  frontmatter (version, title, short description, dependencies, status) and
  task/subtask checkboxes. Update status as work lands.
status: in-progress
depends_on: []
tags: [plan, index]
---

# YMX v1 implementation milestones

Milestones are ordered by dependency; later ones build on earlier outputs.
Status legend: `planned` -> `in-progress` -> `done` (or `blocked`).

| Version | Status | Title | Crate(s) | File | Depends on |
|---------|--------|-------|----------|------|------------|
| 1.1 | `done` | Workspace & crate scaffolding | (all) | [1.1-scaffolding.md](1.1-scaffolding.md) | — |
| 1.2 | `done` | Core IR & diagnostics types | ymx-core | [1.2-core-types.md](1.2-core-types.md) | 1.1 |
| 1.3 | `done` | Project loading & namespace resolution | ymx-lib, ymx-core | [1.3-loading.md](1.3-loading.md) | 1.2 |
| 1.4 | `done` | Front-matter config & entry-path resolution | ymx-config | [1.4-config.md](1.4-config.md) | 1.3 |
| 1.5 | `done` | Interpolation & math engine | ymx-core | [1.5-interpolation-math.md](1.5-interpolation-math.md) | 1.2 |
| 1.6 | `done` | Resolver core (rules 1–11) | ymx-core | [1.6-resolver-core.md](1.6-resolver-core.md) | 1.4, 1.5 |
| 1.7 | `done` | Array templates (rules 12–14) | ymx-core | [1.7-array-templates.md](1.7-array-templates.md) | 1.6 |
| 1.8 | `done` | Builtins: $merge / $map / $reduce (rules 15–16) | ymx-core | [1.8-builtins.md](1.8-builtins.md) | 1.6 |
| 1.9 | `done` | Test harness (ymx-test) | ymx-test | [1.9-test-harness.md](1.9-test-harness.md) | 1.6 |
| 1.10 | `done` | CLI (ymx-cli) | ymx-cli | [1.10-cli.md](1.10-cli.md) | 1.8, 1.9 |
| 1.11 | `planned` | Scenario suite & docs | tests/ | [1.11-scenarios-docs.md](1.11-scenarios-docs.md) | 1.10 |
| 1.12 | `done` | CI gate & dependency security | (repo) | [1.12-ci-gate.md](1.12-ci-gate.md) | 1.1 |
| 1.13 | `done` | Code coverage reporting (cargo-llvm-cov) | (repo) | [1.13-coverage.md](1.13-coverage.md) | 1.9 |
| 1.14 | `done` | Build-time & binary-size metrics | (repo) | [1.14-build-metrics.md](1.14-build-metrics.md) | 1.10 |

## Cross-cutting notes

- **MSRV**: pin latest stable in `rust-toolchain.toml`.
- **Edition**: 2021, declared in `[workspace.package]` of the root `Cargo.toml`.
- **No I/O in ymx-core**: all filesystem work lives in `ymx-lib::load_project`.
- **`load_project` is all-or-nothing**: any load diagnostic -> `Err`, no `Project`.
- **Entry path model**: `<folder.path>.<file>.<component>` (default `main.main`); distinct from `from`/`$name` namespace dotted paths.
- **Templates namespaced by default**; `_ymx.plain` / `--plain` / `--plain-template` promote to global.
- **Diagnostic carries its resolved file path** (`Option<PathBuf>`) so load-errors render without a `Project`.
- Run `cargo fmt && cargo clippy --workspace --all-targets` and `cargo test --workspace` before declaring a milestone done.
- **Version tagging**: `/ymx-build` creates an **annotated** git tag `v<version>` (e.g. `v1.2`) at **gatekeeper PASS** — the moment implementation is verified. Tag semantics = "implementation-verified complete"; the frontmatter `status: done` flip is a follow-up via `/ymx-update` (separate commit). Never force-move or delete a version tag. No remote configured today → tags are local; `git push origin v<version>` once a remote exists.
