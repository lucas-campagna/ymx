# YMX — Product Requirements Document

> Split into modular files under [`docs/PRD/`](PRD/).

## Contents

| File | Section |
|------|---------|
| [Purpose & Terminology](PRD/01-purpose.md) | Project purpose, terminology |
| [Technologies](PRD/02-technologies.md) | Rust, toolchain, serialization |
| [Scope](PRD/03-scope.md) | v1, v2, v3, future |
| [Architecture](PRD/04-architecture.md) | Crate layout, diagnostic codes |
| [Multi-file projects](PRD/05-multi-file.md) | `_use` directive, imports |
| [Project metadata](PRD/06-project-metadata.md) | `_ymx`, `_test`, `_use` details |
| [CLI](PRD/07-cli.md) | Flags, stdin modes, exit codes |
| [Library API](PRD/08-library-api.md) | ymx-core, ymx-lib, ymx-config, ymx-test, ymx-cli |
| [Features](PRD/09-features.md) | v1 feature list, future |
| [Testing](PRD/10-testing.md) | Scenario layout, harness |
| [Rules: String syntax & Visibility](PRD/11-rules-string.md) | Interpolation, escaping, component visibility |
| [Rules 1–8](PRD/12-rules-1-8.md) | Components, calls, templates, from, math, shortcut |
| [Rules 9–14](PRD/13-rules-9-14.md) | Required props, resolution pipeline, array templates |
| [Rules 15–18](PRD/14-rules-15-18.md) | Builtins ($map/$reduce/$merge), optional (?), math shorthand ($) |
| [Rules 19–22](PRD/15-rules-19-22.md) | Shell components, HTML renderer, IPC, brace calls |
