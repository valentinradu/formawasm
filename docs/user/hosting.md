# Hosting a Component

Once `formawasm` has emitted a `.wasm` component, you need a runtime to execute it. This chapter walks through the [`wasmtime`](https://docs.rs/wasmtime) Rust API end-to-end; the same component can also be loaded by browser-based runtimes ([`jco`](https://github.com/bytecodealliance/jco)), `wasmi`, or any other Component-Model-compliant engine.

## Minimal: call an exported function

Given an `id.wasm` from the [Quickstart](quickstart.md):

```rust
use wasmtime::{Config, Engine, Store};
use wasmtime::component::{Component, Linker};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new();
    config.wasm_component_model(true);   // required
    let engine = Engine::new(&config)?;

    let bytes = std::fs::read("id.wasm")?;
    let component = Component::from_binary(&engine, &bytes)?;

    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;

    let id = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "id")?;
    let (got,) = id.call(&mut store, (42,))?;
    assert_eq!(got, 42);
    Ok(())
}
```

A few notes:

- `wasm_component_model(true)` is **required** — the default `wasmtime::Engine` only loads core wasm modules.
- Function names cross the boundary in **kebab-case**. A formalang `pub fn call_host(...)` is `call-host` at the WIT layer and `call-host` in `get_typed_func`.
- Argument and return types are tuples. A function returning a single value still surfaces as `(T,)`.

## Supplying host imports

Any formalang `extern fn` becomes a wasm import that the host must provide before instantiation:

```rust
use wasmtime::component::Linker;

let mut linker = Linker::<()>::new(&engine);

// World-level imports show up at the linker's root namespace
// under their kebab-case WIT name.
linker
    .root()
    .func_wrap("host-double", |_store, (n,): (i32,)| Ok((2_i32 * n,)))?;

let instance = linker.instantiate(&mut store, &component)?;
let call_host = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "call-host")?;
let (got,) = call_host.call(&mut store, (21,))?;
assert_eq!(got, 42);
```

If the host doesn't supply every import the component declares, `linker.instantiate(...)` returns an error.

## Strings, lists, and records

For non-primitive boundary types, prefer [`wasmtime::component::bindgen!`](https://docs.rs/wasmtime/latest/wasmtime/component/macro.bindgen.html) to generate strongly-typed Rust wrappers from the WIT file. The dynamic `get_typed_func` API works for primitives, but `bindgen!` handles records, variants, lists, and options without you spelling out the canonical-ABI layout.

```rust
wasmtime::component::bindgen!({
    path: "wit/component.wit",
    world: "component",
});
```

Then call exports through the generated trait surface with native Rust types.

## Picking a runtime

| Runtime | Strengths | Where it's at home |
|---|---|---|
| [`wasmtime`](https://docs.rs/wasmtime) | First-class Component Model, JIT + AOT | Server-side, CLIs, embedded Rust applications |
| [`wasmi`](https://github.com/wasmi-labs/wasmi) | Pure-Rust interpreter, `no_std` | Resource-constrained or sandboxing-sensitive embeddings |
| [`jco`](https://github.com/bytecodealliance/jco) | JavaScript/TypeScript host bindings | Browser, Node.js |
| Browsers (with `js-component-tools`) | Native execution in the page | Web frontends |

formawasm-emitted components are runtime-agnostic; the choice is yours and depends on where the component runs.

## Zero-export components

A formalang module without any `pub fn` (or with only `pub struct` / `pub enum` declarations) emits a valid component with no exports. It's still loadable, just not callable from the host. Useful for type-only modules — see the WIT examples in [Type Mapping](type-mapping.md).
