# YMX

A Yaml WEB/CLI/Library parser to docs/html/pdf.

## Purpose of the project

Make use of the easy reading and write of yaml files to create a parser for any other documents.

This project will provide a tool/compiler to build documents, pdfs and even html from yaml.

## Technologies

We are going to just use Rust, since there's no need for GC and should be a long live project, with type and memory safety.


## Features

- Compiling bug report showing the line, column and component name where the issue is
- Compiling flags
- Compiling project (many files)

### Compiling Rules

Here are the rules of this project:

#### 1. Every key-value in the main document is a component with name given by the key and content given by the name

#### 2. Components are callable with arguments, arguments are texts starting with `$` (can contain `_`)

Example:

```yml
user:
  name: $user_name
  phone: $user_phone
```
```
```

Calling `user` with `user_name="Mathew"` and `user_phone=123456789` you will get the object `{"name": "Mathew", "phone": 123456789}`.

> Notice that values are parsed with string as fallback.

#### 3. Components can call each other with the `from` property (user can use override this keyword to avoid conflicts on his context)

Example:

```yml
CompA:
  from: CompB
  x: 12
  y: 34
CompB: $x + $y
```

Calling `CompA` we get `"12 + 34"`.

#### 4. Component calling can also be made using `$`

Example:

```yml
a: $b(x=12,y=34)
b: $x + $y
```

> This way instead of interpret `$b` as a property of `a`, we are calling `b` component from `a` body.

#### 4. Component calling with `$` can be made with unamed properties, this way we use sequence properties `$0`, `$1`, `$2`, ...

Example:

```yml
a: $b(12,34)
b: $0 + $1
```

Calling `a` we get `"12 + 34"` again.

#### 5. We can do math and call components as functions with `${...}`

Example:

```yml
a: $b(12,34)
b: ${$0 + $1}
```

Calling `a` now we get the number`46`.

We could also call a component

Example:

```yml
a: ${b(12,34) + c(28)}
b: ${$0 + $1}
c: ${2 * $0}
```

Here, `a` calls `b` which sums `12` with `34` and return `46` and `c` is called with `28` which doubles to `56`, then at `a` we sum `46 + 56` then results in `102`.

> The math operators are:
> `+` (Addition): Sums two numbers or concatenates strings.
> `-` (Subtraction): Subtracts the right value from the left.
> `*` (Multiplication): Multiplies two values.
> `/` (Division): Divides the left value by the right.
> `%` (Remainder/Modulus): Returns the integer remainder of division.
> `**` (Exponentiation): Raises the first operand to the power of the second.

#### 6. You can shortcut component calling by using the component name in the property

```yml
a:
  b: 1
  y: 3
  z: 5
b: [$default,$y,$z]
```

Calling `a` we get `[1,3,5]`, the `default` will be the value in front of the component name, it can be configurable to use another value.

#### 7. Components can have template components which will be called after the component automatically

```yml
$box:
  from: div
  children: Hello, $name!
box:
  name: Sir. $name
```

Calling `box` with `{"name": "Rocky"}` we get `{"from": "div", "children": "Hello, Sir. Rocky"}`. The `"Rocky"` is applied to `box` in it's call, then `box` calls `$box` which expects a `$name` property.

#### 8. Non existing properties in the calling component are ignored

```yml
a: $x + $y
```

Calling `a` with `{"a":1,"b":2,"c":3}` we get `"1 + 2"`, that is, `"c"` is ignored.

#### 9. All properties are required

#### 10. An array component maps into its template component

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

Calling `a` it's mapped into it's template and we get `[{"prop1": 2, "prop2": 2}, {"prop1": 4, "prop2": 12}]`.

Example 2

```yml
$a: $x + $y
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` we get `["1 + 2", "3 + 4"]`.

Example 2

```yml
$a:
  - "values are $x and $y"
  - {x:$y,y:$x}
a:
  - x: 1
    y: 2
  - x: 3
    y: 4
```

Calling `a` we get `[["values are 1 and 2", {"x": 2, "y": 1}], ["values are 3 and 4", {"x": 4, "y": 3}]]`.

#### 11. An array template component reduces, starting with it's decendent component

During the iteration we have the auxiliar variable `$last` which returns the result of the previous item and the initial value is always preserved, but are overwritten on item only.

Example

```yml
a:
  x: 1
  y: 2
$a:
  - x: ${x + 1}
    y: ${y + 2}
  - ${x + $y}
  - $x + $y = $last
   
```

Calling `a` it iterates through it's template component calling each item with the arguments so, the iteration result items are `{"x": 2, "y": 4}` for the first, then the second will return the number `6` (sum of 2 with 4) and the third and last item we have as input `$last = 6`, `$x = 1` and `$y = 2` (since `$a` was called with `x=1` and `y=2`), and the output will be the string `"1 + 2 = 6"`. So for short, calling `a` we get `"1 + 2 = 6"`.

#### 12. A map and a reduce can be performed through `$map(object,array)` and `$reduce(object,array)`

Example

```yml
a: $a + $b
b:
  - {a: 1, b: 2}
  - {a: 2, b: 3}
c: $map(a,b)
```

Calling `c` we get `["1 2", "2 3"]`.

Example

```yml
a: $a + $b
b:
  - {a: 1, b: 2}
  - $last = ${last}
c: $reduce(a,b)
```

Calling `c` we get `"1 + 2 = 3"`, because the first item calls component `a` with `a=1` and `b=2` and returns `"1 + 2"` that is returned into the second (and last) item.

#### 13. You can merge an object or an array using `$merge(a, b)`

Example 1

```yml
a: [1,2,3]
b: [4,5,6]
c: $merge(a,b)
```

Calling `c` we get `[1, 2, 3, 4, 5, 6]`.

Example 2

```yml
a: {a:1,b:0}
b: {b:2,c:3}
c: $merge(a,b)
```

Calling `c` we get `{"a": 1, "b": 2, "c": 3}`.
