# YMX

A YAML parser and compiler for documents, HTML, and PDF — usable as a CLI tool, a WEB service, and a library.

## Purpose of the project

YAML is human-friendly to read and write. YMX uses that property to let authors describe rich, reusable, composable documents that compile to HTML, PDF, or arbitrary JSON-like structures.

The project provides a tool/compiler that turns YAML source files into documents, PDFs, and HTML, while keeping the authoring experience simple and declarative.

## Terminology

- **Document**: a single YAML source file parsed by YMX.
- **Component**: each top-level key-value pair in a document defines a component. The key is the component's name and the value gives its content (rule 1).
- **Property**: a key-value pair inside a component. Properties are also the arguments the component accepts when called.
- **Argument**: a value passed to a component when it is called. Arguments are referenced in component bodies as `$name` (named) or `$0`, `$1`, `$2`, … (positional).
- **Template component**: a component whose name starts with `$` (e.g. `$box`). Templates are applied automatically after the component that uses them is called (rule 8). Templates can chain indefinitely (`$a`, `$$a`, `$$$a`, …).
- **Entry**: the top-level component chosen for compilation. Defaults to `main`; overridable with `--entry`.
- **Namespace**: the scope a component lives in. The project root is the global namespace; each subdirectory is a sub-namespace addressed by a dotted path (e.g. `subdir.comp`).

## Technologies

The project is written in Rust. Rust provides type and memory safety without a garbage collector, which suits a long-lived, performance-sensitive tool.

## Scope

YMX is being built in versions. The rules in this document describe the language and are stable across versions; the *output targets* arrive incrementally.

**v1 (current)**: the resolver for rules 1–15, emits JSON. CLI and library only. HTML, PDF, and WEB are intentionally not in v1.

**v2**: HTML renderer + CLI flag to pick the target.

**v3**: PDF renderer (backend choice deferred until needed).

**Future**: WEB service; swappable math/engine backends (Lua, Python, JavaScript); user-defined builtins via a plugin system.

## Architecture

The project is a Cargo workspace of multiple crates:

```
ymx/
├── Cargo.toml
├── crates/
│   ├── ymx-core/    # parser, resolver, diagnostics, builtins (no I/O)
│   ├── ymx-lib/     # re-exports ymx-core as stable public API
│   ├── ymx-cli/     # binary: arg parsing, dir walking, JSON emit
│   └── ymx-web/     # stub crate (v3+)
└── tests/
    └── cases/
        └── rule-NN/<scenario>.yml   # insta snapshots
```

- **YAML parsing**: `yaml-rust2` is used directly so source spans (line/column) are preserved on every scalar for diagnostics.
- **Output**: YAML → intermediate JSON-like `Value` IR → serialize to JSON (v1). HTML/PDF renderers consume the same IR in later versions.
- **Math**: a `MathEngine` trait evaluates `${...}`. v1 uses dynamic numeric coercion (operands parse as numbers when possible; `+` falls back to string concatenation otherwise). The trait is the boundary for swapping to a Lua/Python/JavaScript engine in the future.
- **Builtins**: a `Builtin` trait. v1 ships `$map`, `$reduce`, `$merge`. The trait is the future plugin boundary.
- **Diagnostics**: structured `Diagnostic { line, col, component, code, message }` rendered to stderr. Designed so a richer "bug report" mode can be added later without breaking the API.
- **Cycles**: no precise cycle detection in v1; a configurable depth cap (`--max-depth`, default 256) prevents runaway recursion and surfaces as a "max depth exceeded" diagnostic.

## Multi-file projects

A project is a directory. Namespaces are directory-scoped:

- Top-level files in the project root share one global namespace.
- Subdirectories form sub-namespaces, accessed via a dotted path (e.g. `subdir.comp`).
- Two definitions of the same component name in the same namespace are a hard error.
- Each `.yml` / `.yaml` file is one document. Multi-document YAML streams (`---`) inside a single file are not supported in v1.

## CLI

```
ymx <path> [flags]
```

- `--entry <name>`: component to compile (default `main`).
- `--from-keyword <kw>`: override the `from` keyword (default `from`).
- `--default-keyword <kw>`: override the `$default` keyword (default `default`).
- `--max-depth <n>`: limit on template/call recursion (default `256`).
- `--output <file>`: write JSON to a file instead of stdout.
- `--pretty`: pretty-print the JSON output.
- `--format <json|diagnostics>`: output style (v1: `json`; `diagnostics` lists errors only).

## Features

**v1**

- Compile a directory of YAML files into a JSON document, applying rules 1–15.
- Configurable compile flags (see CLI).
- Compile multi-file, directory-scoped projects.
- Structured diagnostics reporting line, column, and component name where an issue occurred, plus an error code.
- Usable as a CLI tool and as a Rust library (`ymx-lib`).

**Later**

- HTML and PDF renderers.
- WEB service (REST endpoint that compiles submitted YAML).
- Swappable math/engine backends.
- User-defined builtins via a plugin system.
- Rich "bug report" mode with full call-stack and local-argument dump.

## Compiling Rules

The following rules define how YMX parses and resolves components.

### 1. Top-level keys are components

Every key-value pair in the main document is a component. The key gives the component's name; the value gives its content.

### 2. Components are callable with arguments

Arguments are referenced in a component body by a name starting with `$` (the name may contain `_`).

Example:

```yml
user:
  name: $user_name
  phone: $user_phone
```

Calling `user` with `user_name="Mathew"` and `user_phone=123456789` produces the object `{"name": "Mathew", "phone": 123456789}`.

> Note: argument values are parsed, falling back to string when no other type matches.

### 3. Components can call each other with the `from` property

The `from` property references another component by name. Users can override this keyword in their own context to avoid conflicts.

Example:

```yml
CompA:
  from: CompB
  x: 12
  y: 34
CompB: $x + $y
```

Calling `CompA` returns `"12 + 34"`.

### 4. Components can be called inline using `$`

A `$name(...)` expression inside a value calls another component instead of reading a property.

Example:

```yml
a: $b(x=12,y=34)
b: $x + $y
```

Here `$b` is treated as a call to the `b` component from inside `a`'s body, rather than as a property of `a`.

### 5. Positional arguments are supported with `$0`, `$1`, `$2`, …

When calling a component with `$`, arguments may be unnamed. They become sequence properties `$0`, `$1`, `$2`, … inside the called component.

Example:

```yml
a: $b(12,34)
b: $0 + $1
```

Calling `a` returns `"12 + 34"` again.

### 6. Math and component calls with `${...}`

The `${...}` form evaluates its contents as a math expression and can also call components as functions inside it.

Example:

```yml
a: $b(12,34)
b: ${$0 + $1}
```

Calling `a` returns the number `46`.

Components can also be called inside the expression:

```yml
a: ${b(12,34) + c(28)}
b: ${$0 + $1}
c: ${2 * $0}
```

Here `a` calls `b` which sums `12` with `34` yielding `46`, then calls `c` with `28` which doubles it to `56`. Finally `a` sums them to `102`.

> The math operators are:
> `+` (Addition): Sums two numbers or concatenates strings.
> `-` (Subtraction): Subtracts the right value from the left.
> `*` (Multiplication): Multiplies two values.
> `/` (Division): Divides the left value by the right.
> `%` (Remainder/Modulus): Returns the integer remainder of division.
> `**` (Exponentiation): Raises the first operand to the power of the second.

### 7. Shortcut: a property name matching a component name calls that component

If a component defines a property whose name matches another component, that property value is passed to the matched component as `$default`, and the remaining properties of the calling component are passed as arguments.

Example:

```yml
a:
  b: 1
  y: 3
  z: 5
b: [$default,$y,$z]
```

Calling `a` returns `[1, 3, 5]`. The leading value passed through `$default` corresponds to the property named after the target component; its name is configurable.

### 8. Components can have template components

A component whose name starts with `$` is a template. When a component with a matching (non-`$`) name is called, the template is applied afterwards automatically.

```yml
$box:
  from: div
  children: Hello, $name!
box:
  name: Sir. $name
```

Calling `box` with `{"name": "Rocky"}` produces `{"from": "div", "children": "Hello, Sir. Rocky"}`. The argument `name="Rocky"` is applied to `box`; `box` then invokes `$box`, which expects a `$name` property.

Templates can be chained indefinitely: a component `a` invokes `$a`, which itself can have a template `$$a`, which can have `$$$a`, and so on. The chain unwinds in order — the innermost template is applied first, then its result feeds the next template, until no more templates match.

When a component's result is a scalar (not an object with named properties), it is passed to the next template as the positional argument `$0`, consistent with rule 5.

Example

```yml
$$$a: "final: $0"
$$a:  $0 + 1
$a:   $0 * 2
a:    10
```

Calling `a`:

1. `a` returns `10` to its template `$a`.
2. `$a` runs `$0 * 2` with `$0 = 10`, returning `20` to `$$a`.
3. `$$a` runs `$0 + 1` with `$0 = 20`, returning `21` to `$$$a`.
4. `$$$a` runs `"final: $0"` with `$0 = 21`, returning `"final: 21"`.

So calling `a` returns `"final: 21"`.

### 9. Non-existing properties in the calling component are ignored

```yml
a: $x + $y
```

Calling `a` with `{"a":1,"b":2,"c":3}` returns `"1 + 2"`; the `c` property is ignored because `a` does not reference it.

### 10. All referenced properties are required

Any property referenced by a component (via `$name`, `$0`, `${name}`, etc.) must be supplied when the component is called. Unknown/extra properties are ignored per rule 9.

### 11. An array component maps over its template

When a component is an array and a matching `$template` exists, each item of the array is passed through the template, producing one output item per input item.

Example 1

```yml
$a:
  prop1: ${x + 1}
  prop2: ${y * x}
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` produces `[{"prop1": 2, "prop2": 2}, {"prop1": 4, "prop2": 12}]`.

Example 2

```yml
$a: $x + $y
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` produces `["1 + 2", "3 + 4"]`.

### 12. An array template component reduces over its sibling component

When the template is an array, the sibling component supplies the initial arguments. The template iterates over its own items; on each step the previous item's result is available as `$last`.

Argument overwrite rule: each step starts from the **initial** arguments. The previous step's result **only** overwrites the initial for the **immediately next** step, and **only** for the keys it actually returns. If the previous step returned a non-object (a number, a string, an array, …), no overwrite happens and the next step reverts to the initial arguments.

Example

```yml
a:
  x: 1
  y: 2
$a:
  - x: ${x + 1}
    y: ${y + 2}
  - ${x + $y}
  - $x + $y < $last
```

Calling `a`:

1. The first template item runs with the initial `x=1, y=2`, producing `{"x": 2, "y": 4}`. This object returns `x` and `y`, so it overwrites the initial for the next step **only**.
2. The second item runs `${x + $y}` with `x=2, y=4` (overwritten), producing the number `6`. Since it returns a number (not an object with `x`/`y`), no overwrite carries forward.
3. The third item therefore reverts to the **original** initial values `x=1, y=2`, with `$last=6`, producing the string `"1 + 2 < 6"`.

So calling `a` returns `"1 + 2 < 6"`.

### 13. When both the component and its template are arrays, reduce each element independently

When both `a` and `$a` are arrays, `$a` is treated as a reduce sequence (per rule 12) that is applied independently to **each element** of `a`. The result is an array with one reduced entry per element of `a`. Each element of `a` starts its own reduce run with that element's properties as the initial arguments, using the same overwrite and `$last` semantics as rule 12.

Example

```yml
$a:
  - ${x + y}
  - a: $x
    b: $y
    sum: $last
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a`:

1. The first element `a[0]={x:1, y:2}` is reduced through `$a`. The single template item `a: $x, b: $y` is a pass-through with `x=1, y=2`, so the first result is `{"a": 1, "b": 2, "sum": 3}`.
2. The second element `a[1]={x:3, y:4}` is reduced through the same `$a`, producing `{"a": 3, "b": 4, "sum": 7}`.

So calling `a` produces `[{"a": 1, "b": 2, "sum": 3}, {"a": 3, "b": 4, "sum": 7}]`.

### 14. `map` and `reduce` operations via `$map` and `$reduce`

`$map(object, array)` applies an object component to each item of an array, returning an array of results.

Example

```yml
a: $a + $b
b:
  - {a: 1, b: 2}
  - {a: 2, b: 3}
c: $map(a,b)
```

Calling `c` produces `["1 + 2", "2 + 3"]`.

`$reduce(object, array)` works like `$map`, but each item also has access to `$last`, the result of the previous iteration. The final result is the result of the last item.

Inside `${...}` (math context), `$last` is referenced by the bare name `last`: it is math-evaluated. So `${last}` takes the previous result and evaluates it as a math expression.

Example

```yml
a: $a + $b
b:
  - {a: 1, b: 2}
  - $last = ${last}
c: $reduce(a,b)
```

Calling `c` produces `"1 + 2 = 3"`:

1. The first item calls component `a` with `a=1, b=2`, returning the string `"1 + 2"`. This becomes `$last` for the next step.
2. The second (and last) item has body `$last = ${last}`. Substituting `$last` gives `"1 + 2"`, and math-evaluating `last` (the string `"1 + 2"`) gives the number `3`. The result is `"1 + 2 = 3"`.

### 15. Merging objects and arrays with `$merge`

`$merge(a, b)` merges two values. Arrays are concatenated; objects are shallow-merged (later keys overwrite earlier ones).

Example 1 — arrays

```yml
a: [1,2,3]
b: [4,5,6]
c: $merge(a,b)
```

Calling `c` produces `[1, 2, 3, 4, 5, 6]`.

Example 2 — objects

```yml
a: {a:1,b:0}
b: {b:2,c:3}
c: $merge(a,b)
```

Calling `c` produces `{"a": 1, "b": 2, "c": 3}`.
