<p align="center">
  <img src="website/logo-180.png" alt="formawasm" width="160">
</p>

<h1 align="center">formawasm</h1>

**formawasm** compiles a [formalang](https://github.com/valentinradu/formalang) Intermediate Representation (IR) module into a [WebAssembly](https://webassembly.org) **component** — a `.wasm` binary that any standards-compliant runtime can execute.

```text
formalang frontend ──► IrModule ──► formawasm ──► .wasm component ──► host runtime
```

formawasm is a *backend*: it doesn't parse `.fv` source files itself, and it doesn't run the resulting wasm. Both jobs belong to other libraries (formalang for parsing, [`wasmtime`](https://wasmtime.dev) / [`wasmi`](https://github.com/wasmi-labs/wasmi) / a browser engine for execution). formawasm only emits bytes.

The boundary between a component and its host is described in **WIT** — the small Interface Definition Language of the [Component Model](https://github.com/WebAssembly/component-model). formawasm generates the WIT file automatically from the public surface of each IR module; the host never hand-writes WIT.

---

## Quickstart

Install the CLI:

```bash
cargo install --git https://github.com/valentinradu/formawasm formawasm
```

Compile a `.fv` source file:

```bash
echo 'pub fn id(x: I32) -> I32 { x }' > id.fv
formawasm id.fv
# wrote id.wasm (… bytes) from id.fv
```

Inspect the generated WIT interface:

```bash
wasm-tools component wit id.wasm
```

```wit
package formawasm:generated;

world component {
  export id: func(x: s32) -> s32;
}
```

Run it directly with the [wasmtime](https://wasmtime.dev) CLI — no host code required:

```bash
wasmtime run --invoke 'id(42)' id.wasm
# 42
```

Or wire it into your Rust application:

```rust
use wasmtime::{Config, Engine, Store};
use wasmtime::component::{Component, Linker};

let mut config = Config::new();
config.wasm_component_model(true);
let engine = Engine::new(&config)?;

let bytes = std::fs::read("id.wasm")?;
let component = Component::from_binary(&engine, &bytes)?;
let linker = Linker::<()>::new(&engine);
let mut store = Store::new(&engine, ());
let instance = linker.instantiate(&mut store, &component)?;

let id = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "id")?;
let (got,) = id.call(&mut store, (42,))?;
assert_eq!(got, 42);
```

---

## Documentation

The full book lives at [`docs/`](docs/SUMMARY.md) and is built with [mdBook](https://rust-lang.github.io/mdBook/):

```bash
mdbook serve            # local dev server with live reload
```

Highlights:

**For users** (embedding formawasm in a Rust application):

- [Quickstart](docs/user/quickstart.md) — `.fv` → `.wasm` in 30 seconds
- [Using the Library](docs/user/library.md) — `WasmBackend` from your own crate
- [Hosting a Component](docs/user/hosting.md) — running components under wasmtime
- [Boundary Policy](docs/user/boundary.md) — what can cross the WIT boundary
- [Type Mapping](docs/user/type-mapping.md) — formalang ↔ WIT
- [Feature Coverage](docs/user/features.md) — what's supported, per IR variant
- [Cargo Features](docs/user/cargo-features.md) — `wasm-opt`, `dwarf`, validation
- [Troubleshooting](docs/user/troubleshooting.md) — common errors

**For contributors** (extending the backend):

- [Architecture](docs/developer/architecture.md) — the seven-stage pipeline
- [Crate Layout](docs/developer/crate-layout.md) — what's in `src/`
- [Lowering](docs/developer/lowering.md) — memory model, runtime helpers, per-IrExpr lowering
- [Extending the Backend](docs/developer/extending.md) — adding IR variants, runtime helpers, layouts
- [Testing](docs/developer/testing.md) — test conventions and milestone tests
- [Contributing](docs/developer/contributing.md) — code style, quality gates, microcommit cadence

---

## Status

Phases 1 through 5 are closed. The backend produces a Component-Model artifact for every milestone — recursive functions (`fib`), structs + enums + methods (`Counter` / `Action`), arrays + ranges + for-loops (sieve of Eratosthenes), strings + optionals + dictionaries (`greet`), virtual dispatch (`Greet` trait across two impls), and host-provided externs (`call_host(21) → 42` via `host_double`).

See [`CHANGELOG.md`](CHANGELOG.md) for the phase-by-phase history; the "Roadmap" section at the top captures what's left.

---

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
