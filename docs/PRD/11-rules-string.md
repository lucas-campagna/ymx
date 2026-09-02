## Compiling Rules

The following rules define how YMX parses and resolves components.

### String syntax

Inside a string value, `$` triggers interpolation; `\` is the escape character.

- `$name` (where `name` is `[A-Za-z_][A-Za-z0-9_]*`) references a named argument. The `name` grammar does not include `.`; namespace-qualified names (e.g. `subdir.comp`) are not reachable via bare `$name` (see [Multi-file projects](05-multi-file.md)).
- `$0`, `$1`, … `$N` (where `N` is decimal digits) reference positional arguments.
- `${...}` enters math context (rule 7).
- `$name{...}` is a brace call of the component `name` (rule 22); `${...}` (empty name) is the math context (rule 7).
- `\$` produces a literal `$`; `\\` produces a literal `\`. Any other `\X` is a hard error (`E010`).

**Interpolation result type.** When the *entire* value of a property is a single interpolation — `$name`, `$N`, `${...}`, or a `$name{...}` brace call (rule 22) with no surrounding text — the result keeps the interpoland's native type (e.g. `phone: $user_phone` yields `123456789` as Int when the bound argument is an Int). When an interpolation appears with surrounding text, the result is a String in which each interpoland is rendered per the *Number→string rendering* rule below.

**Number→string rendering.** When a number is rendered into text (string interpolation, math string-concat under `+`, or a scalar surfaced inside a larger string):
- Int renders plainly: `20` → `"20"`.
- Float renders with the **same round-trippable rendering used for JSON output of `Value::Float`** (serde_json/ryu-style): integer-valued floats keep a fractional part — `2.0` → `"2.0"`, `2.5` → `"2.5"`, `0.1` → `"0.1"`. A single shared f64 renderer is used for both string interpolation and JSON output; Rust's default `{}` formatting is **not** used (it would drop the fractional part of integer-valued floats, e.g. `2.0` → `"2"`).

Bool renders as `"true"`/`"false"`; Null renders as `"null"`. Objects and arrays have no meaningful string rendering and raise `E011` when interpolated into text.

### Component visibility

By default, components and templates are visible across the entire project (global namespace). A name whose effective identifier starts with `_` is restricted to **file scope** — it is only visible from the document that defines it.

- `_a` is a file-scoped component: only callable from within the same document.
- `$_a` is a file-scoped template: only applied to matching components defined in the same document.
- The same applies to chained templates: `$$a` and `$$$a` chain through the global `a`; `$_a` / `$$_a` chain through the file-scoped `_a`.

Matching between a component and its template is unaffected — `_a` matches `$_a`, just as `a` matches `$a`. They are distinct names: a document may define both `a` (global) and `_a` (file-scoped).

Referencing a file-scoped component from outside its document is a hard error (`E005`).

**Effective identifier.** A component or template name is a leading sequence of `$` characters (the template prefix, zero or more) followed by an *effective identifier*. The effective identifier is `[A-Za-z_][A-Za-z0-9_]*` (letters, digits, underscores; must start with a letter or underscore). A leading `$` count of zero marks a regular component; one or more mark templates of increasing chain depth. The `_`-prefix check for file scope applies to the effective identifier (after stripping any leading `$`s): `$_a` is file-scoped because its effective identifier `_a` starts with `_`.

**Reserved names.** Two kinds of effective identifiers are reserved and not user-definable as components/templates:

1. **Builtin names** — the effective identifiers of all builtin special forms: `map`, `reduce`, `merge` (rules 15–16) plus the 1.34 wave — `split`, `join`, `trim`, `upper`, `lower`, `replace`, `filter`, `sort`, `reverse`, `unique`, `flatten`, `first`, `last`, `slice`, `keys`, `values`, `entries`, `from_entries`, `pick`, `omit`, `type`, `is_array`, `is_object`, `is_string`, `is_number`, `is_null`, `to_string`, `to_number`, `coalesce`, `sum`, `avg`, `min`, `max`, `if`, `when`. Plus the builtin components `sh`/`pw` (rule 19). Defining any component or template whose effective identifier is one of these is a hard error (`E007`), regardless of the leading `$` count. The special forms are always invoked via their `$`-prefixed forms; the builtin components are ordinary namespace entries callable via every call form (rule 19).
2. **Meta keys** — `_ymx` (front matter) and `_test` (tests) — described in [Project metadata](06-project-metadata.md). When present at the top level of a document they are intercepted by the engine as metadata and are **never** registered as components or templates. They are not file-scoped components despite their leading `_`; the `_`-prefix visibility rule simply does not apply to them. The bare top-level keys `_ymx` and `_test` are **consumed** (no error); a user component/template cannot be named `_ymx` or `_test` — any such top-level key is treated as the meta block of that name. A component or template whose **effective identifier** equals `_ymx` or `_test` but which carries one or more leading `$` (e.g. `$_ymx`, `$_test`, `$$_ymx`) is **rejected as a reserved name** (`E015`): only the bare meta keys are consumed, and the leading-`$` variants are not legal user-defined components/templates. Meta extraction is performed by `ymx-config` (`_ymx`) and `ymx-test` (`_test`); `ymx-core` only recognizes the two names, excludes them from the namespace, and stores their raw parsed values on the `Project`.

> A namespace dot (`.`) appears only at the *lookup* layer for `from` targets and math `name(...)` calls (e.g. `from: subdir.comp`); it is not part of an effective identifier and cannot appear inside `$name` interpolation. Cross-namespace component references are reached via `from` (rule 6) or via the math `subdir.comp(...)` form (rule 7), never via bare `$subdir.comp` (which would interpolate `$subdir` then the literal text `.comp`).

**Property-key modifiers.** A *property key* (the left-hand side of a property, inside a component body) may carry one or two trailing modifiers that affect how the property is resolved — they are **stripped before** the property name is used for resolution, matching, or the rule-8 shortcut, so `x?` and `x$` both target the slot named `x`.

- `?` (optional / default-merge — rule 17): `x?: v` declares a default `v` for `x` and activates object-merge mode for the enclosing component.
- `$` (math shorthand — rule 18): `x$: src` ≡ `x: ${src}` — the value is a math source string, evaluated to produce the property value.
- Combination `?$`: `x?$: src` ≡ `x?: ${src}` — an optional property whose default is a math-evaluated expression. `x$?:` is `E010` (wrong order).
- `$<name>` (brace-call shorthand — rule 22): `x$name: <v>` ≡ `x: $name{<v>}` — the value is the payload of a brace call to the component `name` (declared or imported; templates are not valid targets). The empty-name form `x$:` is the math shorthand above.
- Combination `?$<name>`: `x?$name: <v>` ≡ `x?: $name{<v>}` — an optional property whose default is a brace call, lazily evaluated (rule 17). `x$name?:` is `E010` (wrong order).
- The modifiers apply to **all property keys** — ordinary string names, integer positional keys (`0?:`, `1$:` ⇒ default for / math-evaluate `$0`, `$1`), and the template/component-name position (the leading name of a top-level pair: `a$: src` ≡ `a: ${src}`, `a$abc: src` ≡ `a: $abc{src}`). They do **not** apply to the reserved meta fields `_ymx`/`_test` nor to any field *inside* those meta blocks: those are not callable components (no template, no `from`, no `${...}`, no `$N`) and the modifiers are rejected on them (`E010`).
