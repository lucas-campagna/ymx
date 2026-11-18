# @ymx/web

TypeScript/WASM library for YMX - a YAML compiler with JavaScript integration.

## Installation

```bash
npm install @ymx/web
# For PDF generation (optional):
npm install ironpress
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
  # HTML component using literal tags
  card$:
    from: "<div>"
    class: "card"
    children:
      from: "<h2>"
      children: $title
      class: "card-title"
    style: "padding: 1rem; border: 1px solid #ccc;"
`);

// Call with named args
ymx.greet({ name: "World" }); // "Hello, World"

// Call with positional args
ymx.add(1, 2); // 3

// JavaScript context in ${...}
// In browser: ymx.select('#my-button') calls document.querySelector('#my-button')

// HTML output
ymx.set_format(ymx.OutputFormat.Html);
const html = ymx.card({ title: "My Card" });
// Returns: '<div class="card" style="padding: 1rem; border: 1px solid #ccc;"><h2 class="card-title">My Card</h2></div>'

// Or use call_html for one-off HTML calls
const html2 = ymx.call_html("card", { title: "Another Card" });

// PDF generation with ironpress
import initIronpress, { HtmlConverter } from 'ironpress';
await initIronpress();
const converter = new HtmlConverter();
try {
  const pdf = ymx.call_pdf("card", { title: "PDF Card" }, converter);
  // pdf is a Uint8Array - save or download it
  const blob = new Blob([pdf], { type: 'application/pdf' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = 'output.pdf';
  a.click();
} finally {
  converter.free();
}
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

Call a component by name. Returns JSON string result (or HTML if format is set to Html).

- Object = named arguments: `{ a: 1, b: 2 }` → `$a`, `$b`
- Array = positional arguments: `[1, 2]` → `$0`, `$1`

### `ymx.call_with_args(name: string, args: object | array): string`

Explicit call with arguments. Same as `call` but more explicit.

### `ymx.call_html(name: string, args?: object | array): string`

Call a component and return HTML output directly (temporarily sets format to Html).

### `ymx.call_pdf(name: string, args: object | array, converter: IronpressConverter): Uint8Array`

Call a component and return PDF output as `Uint8Array` using ironpress.

Requires the `ironpress` npm package. Pass an initialized `HtmlConverter` instance.

```js
import initIronpress, { HtmlConverter } from 'ironpress';
await initIronpress();
const converter = new HtmlConverter();

const pdf = ymx.call_pdf("my_component", { arg: "value" }, converter);
// pdf is Uint8Array

converter.free(); // Always free the converter when done
```

### `ymx.set_format(format: OutputFormat): OutputFormat`

Set the output format for subsequent calls. Returns the previous format.

- `ymx.OutputFormat.Json` (default) - JSON output
- `ymx.OutputFormat.Html` - HTML output
- `ymx.OutputFormat.Pdf` - PDF output (uses HTML as intermediate)

### `ymx.format(): OutputFormat`

Get the current output format.

### `ymx.last_result(): string | undefined`

Get the last result value as a JSON string.

### `ymx.last_result_html(): string | undefined`

Get the last result value as an HTML string.

### `ymx.components(): string[]`

Get all registered component names.

### `${...}` JavaScript Context

Math expressions evaluate in JavaScript:

- `$0`, `$1`, ... = positional arguments
- `$name` = named argument
- Browser globals available in browser context (`document`, `window`, `fetch`, etc.)
- Node.js globals available in Node.js context (`global`, `process`, `require`, etc.)

### HTML Output

YMX uses the `from` keyword and literal tags (`<tag>`) for HTML generation:

```yaml
my_component$:
  from: "<div>"       # Literal tag (renders as <div>)
  class: "container"  # Class attribute
  style: "..."        # Style attribute
  children:           # Child content
    from: "<span>"
    children: $text
```

### PDF Generation

For PDF generation, use `ymx.call_pdf()` with the `ironpress` npm package:

```bash
npm install ironpress
```

```js
import initIronpress, { HtmlConverter } from 'ironpress';
await initIronpress();
const converter = new HtmlConverter();

try {
  const pdf = ymx.call_pdf("my_component", { ... }, converter);
  // Handle pdf Uint8Array
} finally {
  converter.free();
}
```

The ironpress converter supports page size, margins, headers, footers, and custom fonts. See [ironpress docs](https://gastongouron.github.io/ironpress/) for details.
