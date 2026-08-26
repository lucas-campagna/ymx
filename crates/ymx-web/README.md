# @ymx/web

TypeScript/WASM library for YMX - a YAML compiler with JavaScript integration.

## Installation

```bash
npm install @ymx/web
```

## Usage

```js
import init, { Ymx } from '@ymx/web';
await init();

const ymx = new Ymx();

// Parse YAML components
ymx.parse(`
  greet$: "Hello, " + $name
  add$: $0 + $1
  select$: document.querySelector($0)
`);

// Call with named args
ymx.greet({ name: "World" }); // "Hello, World"

// Call with positional args
ymx.add(1, 2); // 3

// JavaScript context in ${...}
// In browser: ymx.select('#my-button') calls document.querySelector('#my-button')
```

## Targets

- `web` - Browser ESM (default for browser imports)
- `nodejs` - Node.js CommonJS
- `bundler` - Webpack/Rollup/Bun/Vite

## API

### `new Ymx()`

Create a new YMX instance.

### `ymx.parse(code: string): void`

Parse YMX YAML code and register components. Subsequent calls overwrite previous definitions.

### `ymx.call(name: string, args?: object | array): string`

Call a component by name. Returns JSON string result.

- Object = named arguments: `{ a: 1, b: 2 }` → `$a`, `$b`
- Array = positional arguments: `[1, 2]` → `$0`, `$1`

### `${...}` JavaScript Context

Math expressions evaluate in JavaScript:

- `$0`, `$1`, ... = positional arguments
- `$name` = named argument
- Browser globals available in browser context
- Node.js globals available in Node.js context
