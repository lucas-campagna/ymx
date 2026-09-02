## Scope

YMX is being built in versions. The rules in this document describe the language and are stable across versions; the *output targets* arrive incrementally.

**v1 (current)**: the resolver for rules 1–20 (rules 21–22 planned), emits JSON; the HTML (rule 20) and PDF renderers ship feature-gated. CLI and library only.

**v2**: HTML renderer + CLI flag to pick the target.

**v3**: PDF renderer (backend choice deferred until needed).

**Future**: WEB service; swappable math/engine backends (Lua, Python, JavaScript); user-defined builtins via a plugin system.
