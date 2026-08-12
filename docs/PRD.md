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
- **Output**: YAML → intermediate `Value` IR → serialize to JSON (v1). The IR is `Null | Bool | String | Int(i64) | Float(f64) | Array | Object`; object keys preserve YAML insertion order. HTML/PDF renderers consume the same IR in later versions.
- **Math**: a `MathEngine` trait evaluates `${...}`. v1 uses dynamic numeric coercion (operands parse as numbers when possible; `+` falls back to string concatenation otherwise). The trait is the boundary for swapping to a Lua/Python/JavaScript engine in the future.
- **Builtins**: a `Builtin` trait. v1 ships `$map`, `$reduce`, `$merge`. Each builtin is a *special form* that declares its own argument-evaluation strategy (e.g. `$map`/`$reduce` keep their first argument unevaluated as a callable component). The trait is the future plugin boundary.
- **Diagnostics**: structured `Diagnostic { line, col, component, code, message }` rendered to stderr. Designed so a richer "bug report" mode can be added later without breaking the API.
- **Cycles**: no precise cycle detection in v1; a configurable depth cap (`--max-depth`, default 256) prevents runaway recursion and surfaces as a "max depth exceeded" diagnostic.

## Multi-file projects

A project is a directory. Namespaces are directory-scoped:

- Top-level files in the project root share one global namespace.
- Subdirectories form sub-namespaces, accessed via a dotted path (e.g. `subdir.comp`).
- Two definitions of the same component name in the same namespace are a hard error.
- Each `.yml` / `.yaml` file is one document. Multi-document YAML streams (`---`) inside a single file are not supported in v1.
- A component or template whose name starts with `_` is restricted to file scope: it does not participate in the namespace merge and is not visible from other documents.

## CLI

```
ymx <path> [flags]
```

- `--entry <name>`: component to compile (default `main`).
- `--from-keyword <kw>`: override the `from` keyword (default `from`).
- `--default-keyword <kw>`: override the `$default` keyword name (default `default`); the engine always prefixes the name with `$`.
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

### String syntax

Inside a string value, `$` triggers interpolation; `\` is the escape character.

- `$name` (where `name` is `[A-Za-z_][A-Za-z0-9_]*`) references a named argument.
- `$0`, `$1`, … `$N` reference positional arguments.
- `${...}` enters math context (rule 6).
- `\$` produces a literal `$`; `\\` produces a literal `\`.

### Component visibility

By default, components and templates are visible across the entire project (global namespace). A name whose effective identifier starts with `_` is restricted to **file scope** — it is only visible from the document that defines it.

- `_a` is a file-scoped component: only callable from within the same document.
- `$_a` is a file-scoped template: only applied to matching components defined in the same document.
- The same applies to chained templates: `$$a` and `$$$a` chain through the global `a`; `$_a` / `$$_a` chain through the file-scoped `_a`.

Matching between a component and its template is unaffected — `_a` matches `$_a`, just as `a` matches `$a`. They are distinct names: a document may define both `a` (global) and `_a` (file-scoped).

Referencing a file-scoped component from outside its document is a hard error.

### Resolution order

Resolving a component runs in three steps, in this fixed order:

1. **Property resolution (before template)** — every property value of the component is fully resolved. A property value is a *nested call-site* when it is an object containing the `from` key, or any value containing an inline `$comp(...)` call (rule 4) or a `${...}` interpolation (rule 6). Nested call-sites resolve **bottom-up**: the deepest nested call is evaluated first, its return value bubbles up to its parent, and so on, until every property of the component has a fully resolved value. Bare `$name` (no parens) resolves as: (a) a named argument `name` in scope → that argument's value; (b) else if a regular component `name` exists → call it with no args and use its return value (its own template chain applies first); (c) else → hard error (rule 10). `$name(...)` unconditionally calls the component `name` (rule 4) and bypasses the argument lookup. Inside `${...}` (math context) there is **no fallback**: a bare identifier refers to an argument or the math result of the previous step (`last`); to call a component inside math, use the `name(...)` form (rule 6).
2. **Template chain (rule 8)** — applied to the post-step-1 property set. The innermost template runs first, its result feeds the next template, and so on. Templates can only be reached through their **direct** child: `a` invokes `$a`; if `$a` is absent, `a` does **not** skip to `$$a` — the chain is broken at that point. Template names are not valid `from` targets.
3. **`from` dispatch (rule 3, after template)** — if the (post-template) value of `from` names a valid *regular* component, call it with the rest of the property set as arguments; the return value replaces the component's output. If the `from` value does not name a valid regular component (templates excluded), `from` is treated as a plain property — no call, no error.

A nested mini-component (an object whose value uses `from`) receives **only the arguments explicitly written in its body**, resolved against the parent's current arguments. The parent's other arguments are not auto-forwarded; rules 9 and 10 apply within the nested call exactly as they do at the top level. A nested mini-component follows the same three-step evaluation as a top-level component, so it can itself contain nested call-sites.

Inline `$comp(...)` calls (and `${...}` interpolations) run in **step 1**, before templates. `from` dispatch runs in **step 3**, after templates. Recursion (nested calls, template chains, `from` dispatch) is bounded by `--max-depth`; exceeding it raises a "max depth exceeded" diagnostic.

Example 1 — nested call-sites resolve inner-to-outer:

```yml
a:
  from: b
  x: $compC($x)
  y:
    from: compC
    0: $x
  z:
    from: compD
    a:
      from: compE
      ...
```

Evaluating `a`, assuming each `comp*` and `b` resolves to a valid component:

1. Resolve the deepest nested call first: `z.a.from` invokes `compE` with the args written in that nested object (resolved against `z`'s context). Its return value becomes property `a` inside `z`.
2. With property `a` resolved, `z.from` invokes `compD` with the args written in `z`'s body (including the now-resolved `a`).
3. Independently, `y.from` invokes `compC` with `0 = $x` (resolved against `a`'s args).
4. Independently, `x` evaluates the inline `$compC($x)` call.
5. `a` now has fully resolved values for `x`, `y`, and `z`. Its template chain (if any) runs next, then `a.from` invokes `b` using those resolved values as arguments.

Example 2 — inline calls run before templates; `from` runs after:

```yml
a:
  from: $b()
$a:
  from: $from
  x: 2
b: c
c: ${1 + $x}
```

Calling `a`:

1. Property resolution: `from: $b()` calls `b` → `"c"`. `a`'s args are now `{from: "c"}`. (`from: $b` would be equivalent here, since no `b` argument is in scope.)
2. Template `$a` runs with those args: `{from: $from, x: 2}` → `{from: "c", x: 2}`.
3. `from` dispatch: `from="c"` names a valid regular component → call `c` with `{x: 2}`.
4. `c` returns `${1 + 2}` = `3`.

Final result: `3`.

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

> Argument values are parsed at the call site (numbers/strings/null/bool) before being bound to `$N` / `$name` inside the called component.

> Bare `$name` (no parens) resolves, in order: (a) if a named argument `name` is in scope → that argument's value; (b) else if a regular component `name` exists → call it with no args and use its return value; (c) else → hard error (rule 10). This fallback applies wherever `$name` appears, including inside plain strings. `$name(...)` unconditionally calls the component `name` and bypasses the argument lookup; the two forms coincide when no `name` argument is in scope.

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

> If a component has both `from` and a matching `$template`, the template chain applies FIRST, then `from` resolves against the template's result (see *Resolution order*).

> The `from` value is computed as part of property resolution (step 1) and may be any expression that resolves to a component name — e.g. `from: $b()` first evaluates `$b()`, then uses its return value as the `from` target.

> If the (resolved) `from` value does **not** name a valid *regular* component, `from` is treated as a plain property — no call is made and no error is raised. Template names (those starting with `$`) are not valid `from` targets; templates are only reached through the automatic chain (rule 8).

Example — invalid `from` is a plain property:

```yml
a:
  from: b
```

Calling `a` (no `b` component defined) returns `{"from": "b"}`. Adding component `b`:

```yml
a:
  from: b
b: 123
```

Now `a` calls `b` and returns `123`.

### 4. Components can be called inline using `$`

A `$name(...)` expression inside a value calls another component instead of reading a property.

Example:

```yml
a: $b(x=12,y=34)
b: $x + $y
```

Here `$b` is treated as a call to the `b` component from inside `a`'s body, rather than as a property of `a`.

> A `$b(...)` call may mix positional and named arguments, e.g. `$b(12, y=34)`. Positional arguments bind to `$0`, `$1`, …; named arguments bind to `$<name>`.

> `$name(...)` unconditionally calls the component `name`, even if a `name` argument is in scope. Use `$name(...)` to bypass the argument lookup that bare `$name` performs (rule 2). Inline `$comp(...)` calls run during step 1 of *Resolution order* — before templates (rule 8) and before `from` dispatch (rule 3).

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

> Inside `${...}`, bare identifiers refer to arguments (or the math result `last`) — there is **no component fallback** as there is for bare `$name` outside math (rule 2). To call a component inside a math expression, use the `name(...)` form (no `$` prefix), as in `b(12,34)` above.

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

> If more than one property's name matches a component, it is a hard error (ambiguous shortcut).

> The shortcut fires during step 2 of *Resolution order* — against the post-template property set. The shortcut is **suppressed** when the component has a `from` property pointing to a valid regular component: in that case `from` is the call directive, and the otherwise-matching property is passed as a regular argument to the `from`-targeted component. The shortcut applies inside nested mini-components under the same conditions, including the same suppression when a nested `from` is valid. Suppression does **not** happen when `from` is invalid (plain property per rule 3); in that case the shortcut fires normally.

Example 1 — shortcut fires:

```yml
a:
  b: 1
b: ${default + 1}
```

Calling `a` returns `2`.

Example 2 — shortcut suppressed by a valid `from`:

```yml
a:
  from: c
  b: 1
b: ${default + 1}
c: ${b + 2}
```

Calling `a` returns `3`: `from: c` is valid, so `a` calls `c` with `b=1` (rule 7 does not fire).

### 8. Components can have template components

A component whose name starts with `$` is a template. When a component with a matching (non-`$`) name is called, the template is applied afterwards automatically.

> Templates are looked up in the **global namespace** regardless of which namespace the calling component lives in. A template whose effective identifier starts with `_` (e.g. `$_a`) is instead restricted to file scope (see *Component visibility*).

```yml
$box:
  from: div
  children: Hello, $name!
box:
  name: Sir. $name
```

Calling `box` with `{"name": "Rocky"}` produces `{"from": "div", "children": "Hello, Sir. Rocky"}`. The argument `name="Rocky"` is applied to `box`; `box` then invokes `$box`, which expects a `$name` property.

Templates can be chained indefinitely: a component `a` invokes `$a`, which itself can have a template `$$a`, which can have `$$$a`, and so on. The chain unwinds in order — the innermost template is applied first, then its result feeds the next template, until no more templates match.

> Templates can only be reached through their **direct** child: `a` invokes `$a`; if `$a` is absent, `a` does **not** skip to `$$a` — the chain is broken at that point. A template name (starting with `$`) is not a valid `from` target. Template application is step 2 of *Resolution order* — it sits between property resolution (step 1, where inline `$comp(...)` calls run) and `from` dispatch (step 3).

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
  - {sum: ${x + y}, x: 0}
  - ${sum + x + y}
  - a: $x
    b: ${2*y}
    sum: $last
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` runs a three-step reduce of each element through `$a`:

1. The first element `a[0]={x:1, y:2}`:
   - Step 1: `{sum: ${x + y}, x: 0}` runs with the initial `x=1, y=2` → `{"sum": 3, "x": 0}`. This returns an object with `sum` and `x`, so `x` (and `sum`) overwrite the initial for the next step **only**.
   - Step 2: `${sum + x + y}` runs with `x=0` (overwritten), `y=2` (initial), `sum=3` → the number `5`, which becomes `$last`. Since the result is a non-object, no overwrite carries forward.
   - Step 3: `{a: $x, b: ${2*y}, sum: $last}` runs with the original `x=1, y=2` and `$last=5` → `{"a": 1, "b": 4, "sum": 5}`.
2. The second element `a[1]={x:3, y:4}` runs the same three-step reduce:
   - Step 1: `{sum: ${x + y}, x: 0}` → `{"sum": 7, "x": 0}` (overwrites `x`, adds `sum`).
   - Step 2: `${sum + x + y}` with `sum=7, x=0, y=4` → `11` (becomes `$last`). Reverts.
   - Step 3: `{a: $x, b: ${2*y}, sum: $last}` with `x=3, y=4` and `$last=11` → `{"a": 3, "b": 8, "sum": 11}`.

So calling `a` produces `[{"a": 1, "b": 4, "sum": 5}, {"a": 3, "b": 8, "sum": 11}]`.

#### Edge cases (lenient)

The following lenient fallbacks apply to rules 11–13:

- An **empty array** `a` produces an **empty array** output.
- An **empty `$a` template** is a **pass-through**: it returns its input unchanged.
- A **non-array `$a`** applied to a **non-array `a`** simply calls `$a` with `a` as `$0` (per rule 8's chain semantics).

### 14. `map` and `reduce` operations via `$map` and `$reduce`

`$map`, `$reduce`, and `$merge` (rule 15) are **special forms**: each declares its own argument-evaluation strategy, rather than uniformly receiving all arguments pre-evaluated. `$map` and `$reduce` keep their first argument unevaluated (a callable component) and evaluate the array argument eagerly; `$merge` evaluates both arguments eagerly.

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
