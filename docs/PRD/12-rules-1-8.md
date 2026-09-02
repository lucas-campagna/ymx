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

> Bare `$name` (no parens) resolves to the named argument `name` if it is in scope; otherwise it is a hard error (rule 10, `E003`). This applies wherever `$name` appears, including inside plain strings. `$name(...)` unconditionally calls the component `name` and bypasses the argument lookup. Use `$name()` (with empty parens) to call a component without arguments.

### 3. Components can be called inline using `$`

A `$name(...)` expression inside a value calls another component instead of reading a property.

Example:

```yml
a: $b(x=12,y=34)
b: $x + $y
```

Here `$b` is treated as a call to the `b` component from inside `a`'s body, rather than as a property of `a`.

> A `$b(...)` call may mix positional and named arguments, e.g. `$b(12, y=34)`. Positional arguments bind to `$0`, `$1`, …; named arguments bind to `$<name>`.

> `$name(...)` unconditionally calls the component `name`, even if a `name` argument is in scope. Use `$name(...)` to bypass the argument lookup that bare `$name` performs (rule 2). Inline `$comp(...)` calls run during step 1 of rule 11 — before templates (rule 5) and before `from` dispatch (rule 6).

> Call-site argument grammar. The `(...)` of an inline `$name(...)` (rule 3) or math `name(...)` (rule 7) call holds a comma-separated argument list. Each argument is either positional `value` or named `key=value` (where `key` is an effective identifier). Positional args bind to `$0`, `$1`, …; named args bind to `$key`. Mixing positional and named is allowed, but a positional arg may not appear *after* a named one (hard error `E012`); `()` calls with no arguments. The call target of an inline `$name(...)` is a single effective identifier (namespace-local); cross-namespace calls go through the math `name(...)` form, which accepts a dotted path (rule 7), or through `from` (rule 6).

> Argument value parsing. An argument value token is parsed, in order, as Null (`null`/`~`), Bool (`true`/`false`), Int (integer literal), Float (decimal or exponent literal), or String; unquoted text that matches none becomes a String. Single- or double-quoted tokens are always Strings. Argument values may contain nested `$call(...)` and `${...}` (resolved as nested call-sites per rule 11). A direct argument value that is an inline YAML array/object literal (e.g. `$b([1,2])` or `$b({x:1})`) is a hard error (`E013`); build structured arguments with a `from`-mini-component instead.

> **Brace form.** `$name{payload}` (rule 22) is the sibling inline-call form whose single payload may be a structured literal, which the paren form rejects (E013). For scalar and named payloads the two are argument-equivalent: `$b(40)` ≡ `$b{40}` and `$b(c=1, d=2)` ≡ `$b{{c:1, d:2}}`.

### 4. Positional arguments are supported with `$0`, `$1`, `$2`, …

When calling a component with `$`, arguments may be unnamed. They become sequence properties `$0`, `$1`, `$2`, … inside the called component.

Example:

```yml
a: $b(12,34)
b: $0 + $1
```

Calling `a` returns `"12 + 34"` again.

> **Integer property keys are positional slots.** A property whose YAML key is the integer `0`, `1`, `2`, … (a non-negative integer scalar, not the string `"0"`) denotes the positional slot `$N` of the same index: it sets/reads `$0`, `$1`, … exactly. This generalizes rule 11's `0: $x` mini-component usage and the binding above — a component body may provide a default `$N` by writing the integer key, and a call may set a positional slot via the integer key. A string key `"0"` is an ordinary named property, distinct from the integer key `0`. Negative or non-integer keys are ordinary named properties.

### 5. Components can have template components

A component whose name starts with `$` is a template. When a component with a matching (non-`$`) name is called, the template is applied afterwards automatically.

> Templates are **namespaced by default**: a `$box` defined in `subdir/` applies only to components in the `subdir` namespace. A template whose effective identifier starts with `_` (e.g. `$_a`) is restricted to file scope (see [Component visibility](11-rules-string.md#component-visibility)). The `_ymx.plain` flag (CLI `--plain` / `--plain-template`; default `false`) promotes sub-namespace names into the **global** namespace — `true` promotes components **and** templates; `template` promotes templates only. Promoted names participate in global template lookup, bare `$name`, rule-8 shortcut, and `from` resolution. A promoted name that collides with an existing global definition is `E004`; the namespaced qualified path (e.g. `subdir.comp`) remains reachable alongside the promoted bare name. Template-chain lookup (`a` → `$a` → `$$a` → …) consults the component's own namespace first, then global (per `plain`); a broken link stops the chain (a component does **not** skip a missing `$a` to reach `$$a`).

```yml
$box:
  from: div
  children: Hello, $name!
box:
  name: Sir. $name
```

Calling `box` with `{"name": "Rocky"}` produces `{"from": "div", "children": "Hello, Sir. Rocky"}`. The argument `name="Rocky"` is applied to `box`; `box` then invokes `$box`, which expects a `$name` property.

Templates can be chained indefinitely: a component `a` invokes `$a`, which itself can have a template `$$a`, which can have `$$$a`, and so on. The chain unwinds in order — the innermost template is applied first, then its result feeds the next template, until no more templates match.

> Templates can only be reached through their **direct** child: `a` invokes `$a`; if `$a` is absent, `a` does **not** skip to `$$a` — the chain is broken at that point. A template name (starting with `$`) is not a valid `from` target. Template application is step 2 of rule 11 — it sits between property resolution (step 1, where inline `$comp(...)` calls run) and `from` dispatch (step 3).

When a component's result is a scalar (not an object with named properties), it is passed to the next template as the positional argument `$0`, consistent with rule 4. The full argument-passing rule for scalar, object, and array results — including how the *initial* arguments are retained — is specified just below.

**Argument passing between chain steps.** The arguments the *next* template sees are derived from the *initial* arguments (the args the original component was called with), not the previous template's full arg set:

- **Scalar result** → the next template receives the initial arguments with `$0` overwritten by the scalar (other initial keys retained). The scalar does not consume other initial keys.
- **Object result** → the next template receives the initial arguments, **overwritten only** for the keys the object actually returns, and **only** for the immediately next chain step. After that step, the chain reverts to the initial arguments (the overwrite does not propagate further up the chain). A key returned by the template that was not in the initial args is added for that one step only.
- **Array result** → rules 12–14 govern; an array result propagating up a *non-array* chain link is the "mixed-shape" case below.

This mirrors the overwrite-then-revert semantics of rule 13's array reduce, applied one chain link at a time. If the next template references an arg that is neither in the initial set nor overwritten by the current result, it is `E003` (rule 10).

```yml
a:
  x: 1
  y: 2
$a:
  x: ${x + 10}     # returns {x: 11} (only x)
$$a:
  out: $y          # y is still in the initial args → "2"
```

Calling `a`: `$a` returns `{"x": 11}`; `$$a` sees the initial `{x:1, y:2}` with `x` overwritten to `11` for this step, so `$y` → `2`, producing `{"out": 2}`. (Without the overwrite-then-revert rule, `$a`'s `{x:11}`-only result would make `$y` in `$$a` an `E003`.)

Example

```yml
$$$a: "final: $0"
$$a:  $0 + 1
$a:   $0 * 2
a:    10
```

Calling `a`:

1. `a` returns `10` to its template `$a` (initial args here are empty, `$0` is the only slot).
2. `$a` runs `$0 * 2` with `$0 = 10`, returning `20` to `$$a`.
3. `$$a` runs `$0 + 1` with `$0 = 20`, returning `21` to `$$$a`.
4. `$$$a` runs `"final: $0"` with `$0 = 21`, returning `"final: 21"`.

So calling `a` returns `"final: 21"`.

> **Mixed-shape chains (v1 limitation).** A single chain whose links mix the array shape (rules 12–14) with the non-array shape (rule 5) is **not defined in v1** and raises `E010` when the mismatched link is reached — e.g. `$a` is non-array but `$$a` is array, or `$a` returns an array into a non-array `$$a`. The supported chains are: all links non-array (rule 5), or a terminal array-`$a` applied to a component via rules 12/13/14. This is a documented gap pending a concrete use case; revisit in a later version.

> **Merge-mode templates.** A template whose body contains at least one `?:` property activates rule 17's object-merge mode: the caller's supplied properties forward into the output and win over the template body, while the body (and its `?:` defaults) fills the gaps. A template with no `?:` property keeps the rule-5 behavior above (its output is its own resolved body).

### 6. Components can call each other with the `from` property

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

> If a component has both `from` and a matching `$template`, the template chain applies FIRST, then `from` resolves against the template's result (see rule 11).

> The `from` value is computed as part of property resolution (step 1) and may be any expression that resolves to a component name — e.g. `from: $b()` first evaluates `$b()`, then uses its return value as the `from` target.

> If the (resolved) `from` value does **not** name a valid *regular* component, `from` is treated as a plain property — no call is made and no error is raised. Template names (those starting with `$`) are not valid `from` targets; templates are only reached through the automatic chain (rule 5). Namespace-qualified targets are supported: `from: subdir.comp` resolves `comp` in the `subdir` namespace (see [Multi-file projects](05-multi-file.md)). If the resolved `from` value is not a String (a number, bool, null, array, or object) it is also treated as a plain property — no call, no error — consistent with the invalid-target rule above.

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

### 7. Math and component calls with `${...}`

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
> `+` (Addition): Sums two numbers, or (when either operand is a non-numeric String) concatenates — see `+` semantics below.
> `-` (Subtraction): Subtracts the right value from the left (numeric only).
> `*` (Multiplication): Multiplies two values (numeric only).
> `/` (Division): Always floating-point division; the result is a Float. `5 / 2` → `2.5`. Division by zero is `E011`.
> `%` (Remainder/Modulus): Integer remainder of `left % right`, with both operands coerced to Int (Floats truncated toward zero, non-numeric → `E011`). Sign follows the dividend.
> `**` (Exponentiation): Raises the first operand to the power of the second. `Int ** Int(non-negative)` → Int; everything else → Float. Negative or fractional exponents are Float.

> Precedence (highest to lowest): `**` (right-associative) > unary `-` > `* / %` (left-associative) > `+ -` (left-associative). Parentheses group. There are **no** comparison, equality, or boolean operators; `<`, `>`, `=`, `==`, `and`, `or` are literal text in strings (see rule 13's `$x + $y < $last`, which yields the String `"1 + 2 < 6"`).

> `+` semantics: the operands are first *resolved* (per *Math operand resolution* below). Then: if both resolved operands are numbers (Int or Float), numeric addition (promoted to Float if either is Float); if both are Strings, string concatenation; if exactly one is a String and the other is a number, the number is rendered per [Number→string rendering](11-rules-string.md) and string-concatenated. Any other mixture (Bool, Null, Array, Object) is a hard error (`E011`).

> Numeric promotion: Int ⊕ Int = Int (except `/`, always Float, and `**` per above); any Float operand promotes the result of an arithmetic operator to Float. Bool and Null are not numbers and cannot be coerced.

> `${...}` return type: the math expression may evaluate to any `Value`, not just numbers. `${ $0 }` returns the argument unchanged; `${ x + y }` returns a String when `+` concatenates; `${ obj }` (referencing an in-scope object argument) returns that object. The value flows into surrounding interpolation per *String syntax: Interpolation result type*.

> Identifier grammar inside `${...}`. Named arguments are written as **bare identifiers** (`x`, `y`, `default`, `last`, …) *without* a `$` prefix; positional arguments are written `$0`, `$1`, … (a `$` followed by decimal digits); components are called as `name(...)` *without* a `$` prefix, with `name` optionally a dotted namespace path (e.g. `subdir.comp(12,34)`) and `name()` calling a no-arg component. A `$` followed by a letter inside `${...}` (e.g. `${ $x }`) is `E010` — drop the `$` to reference a named argument. A bare identifier resolves to the in-scope argument of that name or to `last`; anything else is `E003`.

> String literals in math (v1): `${...}` does **not** accept quoted string literals in v1; string operands come only from in-scope argument references (which may themselves be Strings, subject to *Math operand resolution* below). A future version may add string literals inside math.

> Math operand resolution (String re-scan). When an operand of a math operator — a bare identifier, `last`, or a `$N` positional — resolves to a **String**, that String is re-scanned as a math expression and evaluated **in the current scope** (the same scope as the enclosing `${...}`, including `last` and all in-scope arguments): `"1 + 2"` → `3`, `"123"` → `123`, `"x + 1"` (with `x` in scope) → `x + 1`. If the String does **not** parse as a math expression (e.g. free text like `"hello world"`), the identifier is left as a plain String operand of the surrounding operator (numeric operators then raise `E011`; `+` concatenates). Non-String operands are used directly. This re-scan is what makes `last` work (rule 16's `${last}` with `$last = "1 + 2"` yields `3`); it applies uniformly to *every* String-valued operand in math, not only to `last`.
>
> Gotcha: because re-scan evaluates in the current scope, a String argument whose content is a bare-identifier-looking token resolves to that identifier. E.g. `${ x }` with `x = "y"` re-scans as `y`, which looks up the argument `y` (→ `E003` if absent). Keep String arguments used in math either numeric or full math expressions; avoid re-using argument names as string contents.

> `last` is available in `${...}` only within a reduce step (rules 13–16). Outside any reduce step — including on the **first** step of a reduce, before any previous result exists — referencing `last` (or `$last` in a plain string) is `E003` (treated as a missing argument; `last` is an ordinary in-scope argument, nothing more). `last` and `$last` are thus **symmetric across reduce contexts**: both the array-template reduce (rule 13) and `$reduce` (rule 16) expose the previous step's result, accessed as `last` in math and `$last` in plain strings.

### 8. Shortcut: a property name matching a component name calls that component

If a component defines a property whose name matches another component, that property value is passed to the matched component as `$0`, and the remaining properties of the calling component are passed as arguments.

Example:

```yml
a:
  b: 1
  y: 3
  z: 5
b: [$0,$y,$z]
```

Calling `a` returns `[1, 3, 5]`. The leading value passed as `$0` corresponds to the property named after the target component.

> If more than one property's name matches a component, it is a hard error (ambiguous shortcut).

> The shortcut fires during step 3 of rule 11 — against the post-template property set, as sugar for `from` (the two are mutually exclusive). The shortcut is **suppressed** when the component has a `from` property pointing to a valid regular component: in that case `from` is the call directive, and the otherwise-matching property is passed as a regular argument to the `from`-targeted component. The shortcut applies inside nested mini-components under the same conditions, including the same suppression when a nested `from` is valid. Suppression does **not** happen when `from` is invalid — in that case `from` is forwarded as a **normal property** alongside the other arguments to the shortcut-matched component, and the shortcut fires normally.

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

Calling `a` returns `3`: `from: c` is valid, so `a` calls `c` with `b=1` (rule 8 does not fire).
