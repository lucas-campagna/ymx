# YMX Built-in Functions Reference

All builtins are invoked via their `$`-prefixed forms (e.g. `$map`, `$reduce`). Defining a user component or template whose effective identifier matches a builtin name is a hard error (`E007`). Builtins are special forms — each declares its own argument-evaluation strategy rather than uniformly pre-evaluating all arguments.

---

## Table of Contents

- [Special Forms (detailed)](#special-forms-detailed)
  - [$map](#mapcallable-array)
  - [$reduce](#reducecallable-array-init)
  - [$merge](#mergea-b)
- [String & Collection Builtins](#string--collection-builtins)
  - [$split](#splitseparator-string)
  - [$join](#joinseparator-array)
  - [$trim](#trimstring)
  - [$upper](#upperstring)
  - [$lower](#lowerstring)
  - [$replace](#replacesearch-replacement-string)
  - [$flatten](#flattenarray)
  - [$unique](#uniquearray)
  - [$sort](#sortarray)
  - [$reverse](#reversearray)
  - [$slice](#slicestart-end-array)
- [Lookup & Selection Builtins](#lookup--selection-builtins)
  - [$keys](#keysobject)
  - [$values](#valuesobject)
  - [$entries](#entriesobject)
  - [$from_entries](#from_entriesarray)
  - [$pick](#pickkeys-object)
  - [$omit](#omitkeys-object)
  - [$first](#firstarray)
  - [$last](#lastarray)
- [Type Checking & Conversion](#type-checking--conversion)
  - [$type](#typevalue)
  - [$is_array](#is_arrayvalue)
  - [$is_object](#is_objectvalue)
  - [$is_string](#is_stringvalue)
  - [$is_number](#is_numbervalue)
  - [$is_null](#is_nullvalue)
  - [$to_string](#to_stringvalue)
  - [$to_number](#to_numbervalue)
- [Aggregation Builtins](#aggregation-builtins)
  - [$sum](#sumarray)
  - [$avg](#avgarray)
  - [$min](#minarray)
  - [$max](#maxarray)
  - [$coalesce](#coalescevalues)
- [Conditional Builtins](#conditional-builtins)
  - [$if](#ifcondition-then-else)
  - [$when](#whencondition-value)
- [Filter & Transform](#filter--transform)
  - [$filter](#filtercallable-array)
- [Builtin Components](#builtin-components)
  - [sh](#sh--pw---shell-components)
  - [pw](#sh--pw---shell-components)

---

## Special Forms (detailed)

These three builtins have fully specified semantics. They are invoked only via their `$`-prefixed paren forms.

### $map(callable, array)

Applies a component to each item of an array, returning an array of results.

**Syntax:** `$map(<component>, <array>)`

**Argument evaluation:**
- First arg (`<component>`): kept **unevaluated** — it is a reference to a component to call per item.
- Second arg (`<array>`): evaluated **eagerly**.

**Item binding:**
- Object item → named arguments (keys become `$<key>`).
- Scalar item → binds `$0`.
- Array item → `E011`.

**Empty array** → empty result.

```yml
double: ${$0 * 2}
items: [1, 2, 3]
result: $map($double, $items)
# result → [2, 4, 6]
```

```yml
add_props: $a + $b
rows:
  - {a: 1, b: 2}
  - {a: 3, b: 4}
result: $map($add_props, $rows)
# result → ["1 + 2", "3 + 4"]
```

The callable may be namespace-qualified:

```yml
result: $map(utils.trim, $names)
```

### $reduce(callable, array, init?)

Applies a component iteratively over an array, accumulating a result.

**Syntax:** `$reduce(<component>, <array>, <init>?)`

**Argument evaluation:** identical to `$map` — first arg unevaluated (callable), second arg eager. `<init>` (third argument) is **eagerly evaluated** if present.

**Item binding:** identical to `$map`.

**`$last` semantics:** the result of the previous iteration is available as `$last` in the next. If `<init>` is supplied, `$last` is bound to `<init>` on the first iteration. If `<init>` is absent, `$last` is **undefined on the first iteration** — referencing it there is `E003`.

Inside `${...}` math context, `last` (bare identifier) refers to `$last` and is subject to math operand resolution (string re-scan).

**Consequences:**
- Single-element array + `init`: first step runs with `$last = init`
- Single-element array + no `init`: first step runs with no `$last` (E003 if callable references `$last`)
- Multiple items + `init`: step 1 has `$last = init`, step 2 has `$last = result_of_step1`, etc.
- Multiple items + no `init`: step 1 has no `$last`, step 2+ have `$last = prev_result`
- Empty array → always `Null` (no step runs)

```yml
inc: ${last + $0}
nums: [1, 2, 3]
result: $reduce($inc, $nums, 0)
# result → 6
# step 1: last=0, $0=1 → 1
# step 2: last=1, $0=2 → 3
# step 3: last=3, $0=3 → 6
```

### $merge(a, b)

Merges two values.

**Syntax:** `$merge(<value>, <value>)`

**Argument evaluation:** **both** arguments are evaluated eagerly.

**Semantics:**
- Arrays → concatenated (first then second).
- Objects → shallow-merged (second overwrites first for shared keys; second's unique keys appended).
- Any other shape combination (Object + Array, Array + Object, scalar + anything) → `E011`.

```yml
a: [1, 2, 3]
b: [4, 5, 6]
c: $merge(a, b)
# c → [1, 2, 3, 4, 5, 6]
```

```yml
defaults: {theme: dark, lang: en, debug: false}
overrides: {debug: true, log: verbose}
config: $merge(defaults, overrides)
# config → {theme: dark, lang: en, debug: true, log: verbose}
```

---

## String & Collection Builtins

### $split(separator, string)

Splits a string into an array by a separator.

**Syntax:** `$split(<separator>, <string>)`

**Semantics:** returns an array of substrings. The separator is a string; the input is split at each occurrence.

```yml
result: $split(",", "a,b,c")
# result → ["a", "b", "c"]
```

### $join(separator, array)

Joins an array into a string with a separator.

**Syntax:** `$join(<separator>, <array>)`

**Semantics:** concatenates array elements (rendered as strings) with the separator between them.

```yml
result: $join(" ", ["hello", "world"])
# result → "hello world"
```

### $trim(string)

Strips leading and trailing whitespace from a string.

**Syntax:** `$trim(<string>)`

```yml
result: $trim("  hello  ")
# result → "hello"
```

### $upper(string)

Converts a string to uppercase.

**Syntax:** `$upper(<string>)`

```yml
result: $upper("hello")
# result → "HELLO"
```

### $lower(string)

Converts a string to lowercase.

**Syntax:** `$lower(<string>)`

```yml
result: $lower("HELLO")
# result → "hello"
```

### $replace(search, replacement, string)

Replaces occurrences of a search string with a replacement.

**Syntax:** `$replace(<search>, <replacement>, <string>)`

```yml
result: $replace("world", "YMX", "hello world")
# result → "hello YMX"
```

### $flatten(array)

Flattens a nested array by one level.

**Syntax:** `$flatten(<array>)`

```yml
result: $flatten([[1, 2], [3, 4]])
# result → [1, 2, 3, 4]
```

### $unique(array)

Removes duplicate elements from an array, preserving order.

**Syntax:** `$unique(<array>)`

```yml
result: $unique([1, 2, 1, 3, 2])
# result → [1, 2, 3]
```

### $sort(array)

Sorts an array in ascending order.

**Syntax:** `$sort(<array>)`

```yml
result: $sort([3, 1, 2])
# result → [1, 2, 3]
```

### $reverse(array)

Reverses the order of elements in an array.

**Syntax:** `$reverse(<array>)`

```yml
result: $reverse([1, 2, 3])
# result → [3, 2, 1]
```

### $slice(start, end, array)

Returns a sub-array from index `start` to `end` (exclusive).

**Syntax:** `$slice(<start>, <end>, <array>)`

```yml
result: $slice(1, 3, [10, 20, 30, 40])
# result → [20, 30]
```

---

## Lookup & Selection Builtins

### $keys(object)

Returns an array of an object's keys.

**Syntax:** `$keys(<object>)`

```yml
result: $keys({a: 1, b: 2})
# result → ["a", "b"]
```

### $values(object)

Returns an array of an object's values.

**Syntax:** `$values(<object>)`

```yml
result: $values({a: 1, b: 2})
# result → [1, 2]
```

### $entries(object)

Returns an array of `[key, value]` pairs.

**Syntax:** `$entries(<object>)`

```yml
result: $entries({a: 1, b: 2})
# result → [["a", 1], ["b", 2]]
```

### $from_entries(array)

Converts an array of `[key, value]` pairs into an object.

**Syntax:** `$from_entries(<array>)`

```yml
result: $from_entries([["a", 1], ["b", 2]])
# result → {a: 1, b: 2}
```

### $pick(keys, object)

Returns a new object containing only the specified keys.

**Syntax:** `$pick(<keys>, <object>)`

```yml
result: $pick(["a", "c"], {a: 1, b: 2, c: 3})
# result → {a: 1, c: 3}
```

### $omit(keys, object)

Returns a new object excluding the specified keys.

**Syntax:** `$omit(<keys>, <object>)`

```yml
result: $omit(["b"], {a: 1, b: 2, c: 3})
# result → {a: 1, c: 3}
```

### $first(array)

Returns the first element of an array.

**Syntax:** `$first(<array>)`

```yml
result: $first([10, 20, 30])
# result → 10
```

### $last(array)

Returns the last element of an array.

**Syntax:** `$last(<array>)`

```yml
result: $last([10, 20, 30])
# result → 30
```

---

## Type Checking & Conversion

### $type(value)

Returns the type of a value as a string.

**Syntax:** `$type(<value>)`

**Returns:** `"array"`, `"object"`, `"string"`, `"number"`, `"bool"`, or `"null"`.

```yml
result: $type([1, 2])
# result → "array"
```

### $is_array(value)

Returns `true` if the value is an array, `false` otherwise.

**Syntax:** `$is_array(<value>)`

### $is_object(value)

Returns `true` if the value is an object, `false` otherwise.

**Syntax:** `$is_object(<value>)`

### $is_string(value)

Returns `true` if the value is a string, `false` otherwise.

**Syntax:** `$is_string(<value>)`

### $is_number(value)

Returns `true` if the value is a number (Int or Float), `false` otherwise.

**Syntax:** `$is_number(<value>)`

### $is_null(value)

Returns `true` if the value is null, `false` otherwise.

**Syntax:** `$is_null(<value>)`

### $to_string(value)

Converts a value to its string representation.

**Syntax:** `$to_string(<value>)`

```yml
result: $to_string(42)
# result → "42"
```

### $to_number(value)

Converts a value to a number. Strings are parsed; other types are coerced where possible.

**Syntax:** `$to_number(<value>)`

```yml
result: $to_number("42")
# result → 42
```

---

## Aggregation Builtins

### $sum(array)

Returns the sum of all numeric elements.

**Syntax:** `$sum(<array>)`

```yml
result: $sum([1, 2, 3])
# result → 6
```

### $avg(array)

Returns the average of all numeric elements.

**Syntax:** `$avg(<array>)`

```yml
result: $avg([1, 2, 3])
# result → 2
```

### $min(array)

Returns the smallest numeric element.

**Syntax:** `$min(<array>)`

```yml
result: $min([3, 1, 2])
# result → 1
```

### $max(array)

Returns the largest numeric element.

**Syntax:** `$max(<array>)`

```yml
result: $max([3, 1, 2])
# result → 3
```

### $coalesce(values)

Returns the first non-null value.

**Syntax:** `$coalesce(<value1>, <value2>, ...)`

```yml
result: $coalesce(null, null, "default")
# result → "default"
```

---

## Conditional Builtins

### $if(condition, then, else)

Returns `then` if `condition` is truthy, `else` otherwise.

**Syntax:** `$if(<condition>, <then>, <else>)`

```yml
result: $if(true, "yes", "no")
# result → "yes"
```

### $when(condition, value)

Returns `value` if `condition` is truthy, `null` otherwise.

**Syntax:** `$when(<condition>, <value>)`

```yml
result: $when(true, "hello")
# result → "hello"
```

---

## Filter & Transform

### $filter(callable, array)

Filters an array, keeping only items for which the callable returns truthy.

**Syntax:** `$filter(<component>, <array>)`

**Argument evaluation:** identical to `$map` — first arg unevaluated (callable), second arg eager.

**Item binding:** identical to `$map` — object item → named args, scalar item → `$0`.

```yml
is_even: ${$0 % 2 == 0}
nums: [1, 2, 3, 4]
result: $filter($is_even, $nums)
# result → [2, 4]
```

---

## Builtin Components

### sh & pw — Shell Components

`sh` and `pw` are **builtin components** (not special forms) — they are ordinary namespace entries callable through every call form: `from: sh`, the rule-8 shortcut, `$sh(...)` parens, `$sh{...}` braces, and math `sh(...)`.

- `sh` → executes via `sh -c`
- `pw` → executes via PowerShell

**Result:** always `{exit_code: Int, stdout: String, stderr: String}`.

```yml
content: $sh{cat path/to/file.txt}
greeting: $pw{Write-Output "Hello World!"}
```

**Shorthand:**

```yml
main$sh: cat path/to/file.txt
# same as
main: $sh{cat path/to/file.txt}
```

**Restriction:** `_ymx.allowed_backends` limits which backends may execute. Non-zero exit codes are part of the result, not errors. `E016` is emitted only when the executor is missing, the backend is disallowed, or spawning fails.

**Wrapping other interpreters:**

```yml
py: $sh{python -c $0}
main: $py{print(1 + 2)}
# → {exit_code: 0, stdout: "3\n", stderr: ""}
```

See [Rules 19–22](../PRD/15-rules-19-22.md) for full shell component semantics.

---

## Notes

- All builtins are invoked via `$`-prefixed forms only. Defining a user component/template with a builtin effective identifier is `E007`.
- The special forms (`$map`, `$reduce`, `$merge`) have custom argument-evaluation strategies. Other builtins evaluate all arguments eagerly.
- The `Builtin` trait in `ymx-core` is the future plugin boundary for user-defined builtins.
- Full specification: [PRD §Rules 15–18](../PRD/14-rules-15-18.md) (special forms), [PRD §Architecture](../PRD/04-architecture.md) (builtin list).
