## Features

**v1**

- Compile a directory of YAML files into a JSON document, applying rules 1–16.
- Configurable compile flags (see CLI).
- Compile multi-file, directory-scoped projects; file-path entry addressing (`main.main`, `a.b.c`) and namespace promotion via `_ymx.plain` / `--plain` / `--plain-template`.
- Structured diagnostics reporting file path, line, column, and component name where an issue occurred, plus an error code.
- Usable as a CLI tool and as a Rust library (`ymx-lib`).
- Inline `_test` blocks (see [Project metadata](06-project-metadata.md)) drive a tests-first development flow via `ymx-test`.
- Shell components (`sh`, `pw`) as builtin components with `_ymx.allowed_backends` restriction (rule 19).
- Brace calls `$name{payload}` and the `$<name>` key suffix — component calls with structured (scalar / object / array) payloads (rule 22).
- External components via `_use` IPC declarations — persistent subprocess sessions (pipe/socket/http transports), text/json modes, request/response hooks, lifecycle hooks, and `_ymx.allowed_ipc` restriction (rule 21).

**Later**

- PDF renderer.
- WEB service (REST endpoint that compiles submitted YAML).
- Swappable math/engine backends.
- User-defined builtins via a plugin system.
- Rich "bug report" mode with full call-stack and local-argument dump.
