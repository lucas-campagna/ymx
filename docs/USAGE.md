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

`$reduce(fn, arr, init?)` applies `fn` cumulatively over `arr`. Each step runs `fn` with the current array item's fields as named args, plus `$last` = the result of the previous step (or `init` for the first step). The final result is the return value of the last step.

- `init` (optional third arg): the initial value for `$last` on the first step. If absent, `$last` is unavailable on the first step.
- Empty array → `null`.
- Scalar array items bind `$0`.

```yml
inc: ${last + $0}
nums: [1, 2, 3]
main: $reduce($inc, $nums, 0)
# → 6  (step 1: last=0, $0=1 → 1;  step 2: last=1, $0=2 → 3;  step 3: last=3, $0=3 → 6)
```

Without `init`, the first step has no `$last`:

```yml
sum_pair: $a + $b
pairs:
  - {a: 1, b: 2}   # first: no $last → 3
  - {a: 10, b: 0}  # second: $last=3 → 10
main: $reduce($sum_pair, $pairs)
# → 10
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

## 14. External components with `_use`

The `_use` directive normally imports components from other `.yml` files. When the RHS is a **mapping** (not a string), it declares an **IPC component** — an external endpoint backed by a subprocess, socket, or HTTP service. IPC declarations are validated at load time; invalid config emits `E010`.

> **Restricting transports.** Set `_ymx.allowed_ipc` to a list of transport names (e.g. `[pipe, http]`) to block others. Using a disallowed transport emits `E018`.

See [PRD Rule 21](PRD/15-rules-19-22.md) for the full specification.

### 14.1 Basic syntax

```yml
_use:
  alias:
    transport: pipe | socket | http
    cmd: [...]          # required unless external: true
    protocol: line      # default
    mode: text          # default
    ...                 # additional config fields
```

The alias becomes a regular component. Calling it sends a request to the external endpoint and returns the response.

### 14.2 Simple example — Python REPL

Spawn a Python process that reads one line, evaluates it, and prints the result as JSON:

```yml
_use:
  py:
    cmd: [python, -u, driver.py]
    transport: pipe
    protocol: line
    mode: json

main:
  py: print(1 + 2)
# → py called with $0="print(1 + 2)" → driver returns "3\n" → parsed as YAML → 3
```

With `mode: json`, all call arguments are serialized as a JSON object (positional args become integer keys `{0: v0, ...}`). The default `mode: text` sends only `$0` as a string.

### 14.3 Persistent shell session (coproc)

Use `coproc` + `request_template` to keep a process alive across calls. The `sentinel` protocol reads until a delimiter, so each call gets a clean response:

```yml
_use:
  sh:
    cmd: [bash]
    transport: pipe
    protocol: sentinel
    request_template: "{$0}\n__DONE__\n"
    reply_until: '^__DONE__$'

main:
  sh: echo session state persists
# → first call spawns bash; subsequent calls reuse the same process
```

The process is spawned lazily on first call and stays alive for the session lifetime. In `--watch` mode, sessions survive across recompiles.

### 14.4 HTTP/REST

For HTTP endpoints, set `transport: http`. The `url` supports `$0`/`$name` interpolation for path parameters:

```yml
_use:
  user:
    transport: http
    method: GET
    url: http://api.example.com/users/{$0}
    query: [verbose]
    headers: {Accept: application/json}

main:
  user: 42
# → GET /users/42?verbose=…
```

`query` lists named args that go into the query string. `body` controls which args fill the request body (default: all args for POST/PUT).

### 14.5 Shell commands with `shell: true`

When `shell: true`, the string `cmd` is run via `sh -c`. Combined with `request_template`, the template is appended to the command string (not sent to stdin):

```yml
_use:
  curl_client:
    cmd: curl
    shell: true
    transport: pipe
    protocol: raw
    request_template: "-s http://localhost:8000/$0"

main:
  curl_client: /
# → shell executes: curl -s http://localhost:8000/
```

### 14.6 Configuration reference

All fields are optional unless noted.

**Runner**

| Field | Type | Default | Notes |
|---|---|---|---|
| `cmd` | string \| list | — | Required unless `external: true`. List = argv, string = whitespace-split (or shell). |
| `shell` | bool | `false` | Run string cmd via `sh -c`. |
| `cwd` | string | inherit | Working directory. |
| `env` | map | inherit | Merged over parent env. |
| `external` | bool | `false` | `true` = don't spawn; connect only (socket/http). |
| `restart` | `never` \| `on-failure` | `never` | Auto-respawn on call failure. |
| `max_restarts` | int | `3` | Cap for `on-failure`. |
| `lazy` | bool | `true` | `false` = spawn at compile start. |
| `stop_signal` | `term` \| `kill` | `term` | Signal on session teardown. |
| `stop_message` | string | — | Sent on stdin before signaling. |
| `stop_timeout` | ms | `2000` | Wait after `stop_message`. |
| `coproc` | bool | — | Enable persistent session. |

**Transport**

| Field | Type | Default | Notes |
|---|---|---|---|
| `transport` | `pipe` \| `socket` \| `http` | `pipe` | `pipe` = stdin/stdout; `socket` = unix/tcp; `http` = request/response. |
| `addr` / `path` | string | — | For `socket`: unix path or `host:port`. |
| `url` | string | — | For `http`: URL template with `$0`/`$name` interpolation. |
| `method` | string | `POST` | For `http`. |
| `headers` | map | — | For `http`. |
| `read_from` | `stdout` \| `stderr` | `stdout` | Pipe transport only. |

**Protocol**

| Field | Type | Default | Notes |
|---|---|---|---|
| `protocol` | `line` \| `sentinel` \| `raw` \| `json` \| `jsonrpc` | `line` | Framing strategy. |
| `request_template` | string | `"{$0}\n"` | Outgoing message wrapper; `$0`/`$name` interpolated. |
| `reply_until` | regex | — | For `sentinel`: read until this matches. |

**Request / Response**

| Field | Type | Default | Notes |
|---|---|---|---|
| `mode` | `text` \| `json` | `text` | `text` = send `$0` as string; `json` = serialize all args. |
| `on_request` | component name | — | Override: call this with args; its result is sent. |
| `parse` | `none` \| `yaml` \| `json` | `yaml` | How the reply string becomes a value. |
| `trim` | bool | `true` | Strip trailing whitespace before parsing. |
| `error_pattern` | regex | — | Reply matching this → `E018`. |
| `envelope` | `payload` \| `full` | `payload` | `full` → `{stdout, stderr}`. |
| `stderr` | `ignore` \| `capture` \| `fail` | `ignore` | Pipe transport only. |
| `on_response` | component name | — | Transform raw reply before returning. |
| `on_error` | component name | — | Called on failure; may return a fallback value. |

**Timeouts**

| Field | Type | Default | Notes |
|---|---|---|---|
| `startup_timeout` | ms | `10000` | Max wait for `ready` after spawn. |
| `ready` | string/regex | — | Must appear on stdout/stderr before first request. |
| `request_timeout` | ms | `30000` | Per call; `0` = unlimited. |

**Lifecycle hooks**

| Field | Type | Default | Notes |
|---|---|---|---|
| `before_start` | shell command | — | Run before spawn (e.g. `docker build`). |
| `after_start` | shell command | — | Run after spawn + ready. |
| `before_stop` | shell command | — | Run before session teardown. |
| `after_stop` | shell command | — | Run after session teardown. |
| `prelude` | string | — | Sent to process immediately after spawn. |

### 14.7 Testing IPC components

IPC components work with `_test` blocks just like regular components:

```yml
_use:
  py:
    cmd: [python, -u, driver.py]
    transport: pipe
    protocol: line
    mode: json

add: ${x + y}

_test:
  - add:
      args: {x: 3, y: 4}
      result: 7
```

> **Note:** IPC calls execute at compile time, so tests that hit real external services may be slow or require network access. Use `ymx --test .` to run them.
