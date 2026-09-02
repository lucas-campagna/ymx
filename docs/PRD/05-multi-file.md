## Multi-file projects

A project consists of a single **entry file** plus any files it references via the `_use` directive. Loading is explicit and import-based — no automatic recursive directory walk by default.

**`_use` — explicit file imports.** `_use` is a meta key (like `_ymx`, `_test`). It lives in the entry file's document and/or in any file loaded via `_use`. It comes in three forms:

- **`*`** (bare string `_use: *`): recursive wildcard — walk all `.yml`/`.yaml` files under the entry file's directory, skipping `.git`, hidden directories, and subdirectories containing no YAML files directly. This is the default when no `_use` is present.
- **Wildcard object** (`_use: {"*": "foo"}`): import all public (non-`_`-prefixed) components and templates from `foo.yml` (or `foo.yaml`) into the global namespace, keeping their original names. The file stem `foo` resolves with the same E009 rules as the entry path.
- **Named imports** (`_use: {x: "foo.bar"}`): import component `bar` from `foo.yml` and make it available as `x` in the global namespace. The RHS is `<file>.<component>` — the file stem must resolve to exactly one `.yml`/`.yaml` file (E009 on ambiguity or missing); the component must exist in that file (E002). Multiple files may define the same component name internally; renaming on import prevents namespace collisions.

All imported components land in the **entry's global namespace**. File-scoped components (`_`-prefixed) are never importable (E005). Components may be renamed on import, so two imported files may both define `main` internally without conflict — as long as the local aliases differ.

**Transitive `_use`**: an imported file's own `_use` is also processed. If `main.yml` imports `utils.yml` and `utils.yml` imports `math.yml`, all components from `math.yml` are available in `main`'s global namespace too. Cycles are detected and raise `E001`.

**Path syntax**: the dotted path in `_use` values (`foo.bar`) follows the same `<file>.<component>` notation as `from:` — the file stem is the first segment, the component is the last. Only the global namespace is reachable via `_use`; sub-namespace prefixes are not allowed.

> **Backward compatibility.** When no `_use` key is present in the entry file, the engine behaves as if `_use: *` were given — a recursive walk of the entry file's directory, with the same filtering rules as the previous automatic behaviour. This means existing single-file projects continue to work unchanged.
