# YMX usage guide

This guide shows real-world YMX patterns with runnable examples. Each example is a self-contained project you can create in a temp directory and compile with `ymx`.

## Project layout

A YMX project is a directory containing `.yml`/`.yaml` files. Each file is a **document**; each top-level key is a **component**. The entry is the component compiled when you run `ymx <dir>`.

```
my-project/
├── main.yml          # entry document (defines `main` component)
├── helpers.yml       # additional components
└── utils/
    └── math.yml      # components in the `utils` sub-namespace
```

## 1. Components and arguments

```yml
# main.yml
greet:
  salutation: $salutation
  name: $name
  message: "$salutation, $name!"

main: $greet

_test:
  greet:
    args: {salutation: "Hello", name: "World"}
    result: {salutation: "Hello", name: "World", message: "Hello, World!"}
```

```bash
ymx --test .   # passes if _test blocks assert true
```

## 2. Templates

A template (`$<name>`) wraps a component's output. It runs automatically after the component, before `from` dispatch.

```yml
# main.yml
main: $card

card:
  name: Alice
  email: alice@example.com

# Apply an HTML template to the card component's output
$card:
  from: div
  class: "card"
  children: "$name ($email)"
```

## 3. Sub-namespaces

Subdirectories form sub-namespaces, accessed via dotted paths.

```bash
# project layout:
#   main.yml
#   utils/strings.yml

# main.yml
main: $greet

greet:
  text: $utils.strings.uppercase($name)
  name: hello

# utils/strings.yml
uppercase:
  text: "${input}"
  input: $text
```

```bash
ymx .   # resolves entry main.main → root main.yml, component main
```

## 4. `from` dispatch

```yml
CompA:
  from: CompB
  x: 12
  y: 34
CompB: $x + $y
# CompA returns "12 + 34"
```

## 5. Math expressions

```yml
main: $area

area:
  pi: 3.14159
  radius: $radius
  result: "${pi * radius ** 2}"
# Calling with radius=5 → result: "78.53975"
```

## 6. $map and $reduce

### $map — transform each array item

```yml
double: ${$0 * 2}
items: [1, 2, 3, 4, 5]
main: $map($double, $items)
# → [2, 4, 6, 8, 10]
```

### $reduce — accumulate with $last

```yml
add: $a + $b
pairs:
  - {a: 1, b: 2}   # first: no $last, result = 3
  - {a: 10, b: 0}  # second: $last = 3, result = 13
main: $reduce($add, $pairs)
# → 13
```

## 7. $merge — combine values

```yml
base:
  theme: dark
  lang: en
overlay:
  theme: light
  debug: true
main: $merge($base, $overlay)
# → {theme: light, lang: en, debug: true}
```

```yml
first: [1, 2, 3]
second: [4, 5]
main: $merge($first, $second)
# → [1, 2, 3, 4, 5]
```

## 8. Optional properties with `?`

```yml
# card with optional avatar URL
$card:
  name: $name
  avatar? : "$cdn/$name.png"
  bio: $bio

main:
  - $card(name=Alice, bio="Engineer")
  - $card(name=Bob, avatar="https://example.com/bob.png", bio="Designer")
```

Without `avatar`, the default `"$cdn/$name.png"` fills the slot. With `avatar` provided, the caller's value wins.

## 9. Namespace promotion with `_ymx.plain`

```bash
# Without plain: subdir/components only visible via dotted path
# With --plain: subdir components promoted to global namespace
ymx . --plain
```

Or in `_ymx`:

```yml
_ymx:
  plain: true   # promote all sub-namespace names to global
```

## 10. Inline tests

```yml
# _test blocks are top-level meta keys, not components
add: ${x + y}
multiply: ${x * y}

_test:
  - add:
      args: {x: 3, y: 4}
      result: 7
  - multiply:
      args: {x: 3, y: 4}
      result: 12
  - add:
      args: {x: 1, y: 2}
      error: E003  # assert a diagnostic code
```

Run: `ymx --test .`

## 11. Library example (Rust)

```rust
use ymx_lib::{load_project, compile, Options};

fn main -> Result<(), Box<dyn std::error::Error>> {
    let project = load_project("./my-project")?;
    let value = compile(&project, &Options::default())?;
    let json = serde_json::to_string_pretty(&value)?;
    println!("{json}");
    Ok(())
}
```

## 12. Library: compile a specific component

```rust
use ymx_lib::{load_project, compile_component, Options, Args};
use ymx_core::Value;

let project = load_project("./my-project")?;
let opts = Options::default();

// Call my_component with named arguments
let value = compile_component(
    &project,
    "my_component",
    &Args::Named(vec![
        ("x".into(), Value::Int(3)),
        ("y".into(), Value::Int(4)),
    ]),
    &opts,
)?;
```

## 13. Library: run inline tests

```rust
use ymx_lib::load_project;
use ymx_config::{extract_options, CliOverrides};
use ymx_test::{parse_tests, run_tests};

let project = load_project("./my-project").unwrap();
let opts = extract_options(&project, &CliOverrides::default_for_tests()).unwrap();
let tests = parse_tests(&project).unwrap();
let results = run_tests(&project, &opts);

let failed: Vec<_> = results.iter().filter(|r| !r.passed).collect();
if !failed.is_empty() {
    for result in &failed {
        eprintln!("FAIL: {} — expected {:?}", result.test.target, result.test.expected);
    }
    std::process::exit(1);
}
```
