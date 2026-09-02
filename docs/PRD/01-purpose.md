# YMX

A YAML parser and compiler for documents, HTML, and PDF — usable as a CLI tool, a WEB service, and a library.

## Purpose of the project

YAML is human-friendly to read and write. YMX uses that property to let authors describe rich, reusable, composable documents that compile to HTML, PDF, or arbitrary JSON-like structures.

The project provides a tool/compiler that turns YAML source files into documents, PDFs, and HTML, while keeping the authoring experience simple and declarative.

## Terminology

- **Document**: a single YAML source file parsed by YMX.
- **Component**: each top-level key-value pair in a document defines a component. The key is the component's name and the value gives its content (rule 1).
- **Builtin component**: an engine-provided component that is always present in the global namespace — `sh`, `pw` (rule 19). Callable through every call form (`from`, the rule-8 shortcut, `$name(...)` parens, `$name{...}` braces, math `name(...)`); its body is implemented by the engine (shell execution via the configured `CommandExecutor`). Distinct from the builtin *special forms* (`$map`, `$reduce`, `$merge`, `$split`, …) which are invoked only via their `$`-prefixed paren forms and reserve their names (`E007`).
- **Brace call**: the `$name{payload}` inline call form (rule 22) — a component call whose single payload may be a structured literal (object/array) that the paren form rejects (E013).
- **Property**: a key-value pair inside a component. Properties are also the arguments the component accepts when called.
- **Argument**: a value passed to a component when it is called. Arguments are referenced in component bodies as `$name` (named) or `$0`, `$1`, `$2`, … (positional).
- **Template component**: a component whose name starts with `$` (e.g. `$box`). Templates are applied automatically after the component that uses them is called (rule 5). Templates can chain indefinitely (`$a`, `$$a`, `$$$a`, …).
- **Entry**: the top-level component chosen for compilation. On the CLI, the mandatory positional argument is a **file** path; `--entry` (default `main`) selects the component within that file. The CLI derives the entry path internally as `<file_stem>.<component>` (always exactly 2 segments: empty folder.path + file stem + component). The entry path is a file-path address, distinct from the namespace dotted path used by `from` / `$name` resolution (see [Multi-file projects](05-multi-file.md)). Defaults to `main.main`; overridable with `--entry`. In library code, `Options.entry` is set directly to the full entry path.
- **Namespace**: the scope a component lives in. The project root is the global namespace; each subdirectory is a sub-namespace addressed by a dotted path (e.g. `subdir.comp`).
- **Meta key**: a reserved top-level key (`_ymx` or `_test`) that is not a component but carries project metadata — front-matter flag defaults or inline tests (see [Project metadata](06-project-metadata.md)).
- **Front matter**: the `_ymx` meta block of a document, supplying compiler-flag defaults.
