# Type Mapping

Every formalang type that crosses the WIT boundary maps to a WIT shape. The full table is below; for the *rules* governing what may cross, see [Boundary Policy](boundary.md).

Internal types (closures, ranges, the bump-allocator's free pointer) live entirely inside the core module and never appear in the WIT file.

## Primitives

| formalang | WIT |
|---|---|
| `I32` / `I64` | `s32` / `s64` |
| `U32` / `U64` | `u32` / `u64` |
| `F32` / `F64` | `f32` / `f64` |
| `Boolean` | `bool` |
| `String`, `Path`, `Regex` | `string` |

## Containers

| formalang | WIT |
|---|---|
| `Optional<T>` | `option<T>` |
| `Array<T>` | `list<T>` |
| `Dictionary<K, V>` | `list<tuple<K, V>>` |
| named tuple `(x: I32, y: I32)` | `record { x: s32, y: s32 }` |

## Aggregates

| formalang | WIT |
|---|---|
| `IrStruct` | `record` |
| `IrEnum` (unit arm) | `variant` arm with no payload |
| `IrEnum` (single-payload arm) | `variant` arm with one payload type |
| `IrEnum` (multi-field payload) | `variant` arm carrying `tuple<T0, T1, …>` |

## Examples

A function with primitives only:

```rust
pub fn id(x: I32) -> I32 { x }
```

emits:

```wit
package formawasm:generated;

world component {
  export id: func(x: s32) -> s32;
}
```

A `pub struct`:

```rust
pub struct Point { x: I32, y: I32 }
```

emits:

```wit
package formawasm:generated;

interface types {
  record point {
    x: s32,
    y: s32,
  }
}

world component {
  use types.{point};
}
```

An enum with mixed arms:

```rust
pub enum Action {
    reset
    add(value: I32)
    replace(x: I32, y: I32)
}
```

emits:

```wit
package formawasm:generated;

interface types {
  variant action {
    reset,
    add(s32),
    replace(tuple<s32, s32>),
  }
}

world component {
  use types.{action};
}
```

## Identifier conversion

formalang identifiers use `snake_case` or `camelCase`; WIT identifiers are required to be **kebab-case**. The backend converts every identifier crossing the boundary:

- `call_host` → `call-host`
- `Point` → `point`
- `MyEnum` → `my-enum`

This applies to function names, type names, record field names, and variant arm names. Inside a component, the original identifier is preserved (and visible in the wasm `name` custom section for debugger tooling); only the boundary view is kebab-cased.

## Internal-only types

These never appear in WIT and are documented here only so you know they exist:

| Type | Internal representation |
|---|---|
| `Range<T>` | `{ start, end }` pair in linear memory |
| `Closure` | After closure-conversion: `(funcref, env_ptr)` pair |
| Anonymous tuple | Anonymous record laid out by the layout planner |
| Vtable | Flat array of `funcref`-table indices, one per trait method |

If you mark a function `pub` whose signature mentions any of these as a top-level type, pre-flight rejects it. (Some — like anonymous structs nested *inside* a `pub` record's fields — flow through transparently because the layout planner handles them.)
