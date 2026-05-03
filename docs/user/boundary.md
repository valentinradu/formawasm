# Boundary Policy

A formalang module compiled by formawasm has two layers:

- **Inside the component**: the full formalang language is supported — closures, generic instantiations (after monomorphisation), all aggregate kinds, virtual dispatch, the works.
- **At the public boundary** (`pub fn`, `extern fn`, `pub struct`, `pub enum` signatures): a strict subset, defined by what WIT can express.

This chapter is about that boundary. The full type-by-type table lives in [Type Mapping](type-mapping.md); this page covers the *rules*.

## What crosses the boundary

| Category | Allowed |
|---|---|
| Primitives | `I32`, `I64`, `F32`, `F64`, `Boolean`, `String`, `Path`, `Regex` |
| Containers | `Optional<T>`, `Array<T>`, `Dictionary<K, V>` (over boundary-allowed `K` / `V`) |
| Aggregates | `IrStruct` → `record`, `IrEnum` → `variant`, named tuples → `record { name: T, … }` |
| Functions | `pub fn` → component **export**, `extern fn` → component **import** |

A few specifics worth highlighting:

- **Multi-field variant payloads** lower as positional `tuple<T0, T1, …>` arms. Field names don't survive the boundary, but the layout planner lays them out in declaration order so the index→field mapping stays stable.
- **Named tuples** map to `record`. formalang tuples carry field names; formawasm deliberately does **not** use WIT's positional `tuple` for top-level tuples — that would lose the names.
- **`Path` and `Regex`** are represented as WIT `string` at the boundary; their identity is preserved internally inside the component.

## Rejected at pre-flight

The pre-flight pass refuses to lower a module that contains any of:

- **Closure-typed values in public signatures.** Closures live entirely inside a component; they can't cross the WIT boundary because there is no canonical-ABI representation for "function pointer plus environment".
- **Generic traits.** Matches the existing `MonomorphisePass` constraint upstream — every trait must be specialized before it reaches the backend.
- **Unresolved type parameters** (`ResolvedType::TypeParam`). `MonomorphisePass` is responsible for stamping these out; if any survive into the backend it's an upstream bug.
- **`ResolvedType::Error`.** Sentinel value — its presence indicates a frontend bug.

Pre-flight failures surface as `WasmBackendError::Preflight(_)` with a typed `PreflightError` payload pointing at the offending IR node.

## Why these rules

The split between "inside the component" and "across the boundary" comes from the [Component Model](https://github.com/WebAssembly/component-model). Core wasm only knows `i32 / i64 / f32 / f64` and linear memory; the Component Model layers a typed ABI on top, and WIT is how that ABI is described. Anything WIT can't represent — closures, unresolved generics, language-internal placeholders — can't cross.

Inside, none of those restrictions apply. The backend has the full power of core wasm available: linear memory, tables, multi-value returns, indirect calls, custom helpers. Closures are lowered through funcref tables, virtual dispatch through per-trait vtables, strings and dictionaries through bump-allocated layouts.

## What this means for your API design

When you decide what to mark `pub`, think about the boundary:

- A **pure-data record or enum** crosses cleanly — `pub struct Point { x: I32, y: I32 }` is fine.
- An **enum with a closure-typed payload** does *not* cross — even if every other variant is plain. Move the closure-bearing variant to a private enum, or accept that the type stays inside.
- A function that **takes or returns a closure** does not cross. Closures are how you compose internal logic, not how you talk to the host.

The internal-vs-public distinction maps onto exactly the same distinction WIT makes between `world` exports and `interface` items not surfaced into the world. formawasm enforces it at `pub`-time so you can't accidentally write a non-portable signature.
