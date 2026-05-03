# Quickstart

The fastest path from a formalang source file to a runnable WebAssembly component is the bundled `formawasm` binary.

## Compile a source file

Save this as `id.fv`:

```rust
pub fn id(x: I32) -> I32 { x }
```

Build the CLI and run it:

```bash
cargo build --release --bin formawasm
./target/release/formawasm id.fv
# wrote id.wasm (… bytes) from id.fv
```

By default the output filename is the input with `.fv` swapped for `.wasm`. Use `-o <path>` to override:

```bash
formawasm id.fv -o build/id.component.wasm
```

If the formalang frontend rejects the source, the diagnostic is printed in the upstream's standard format and the CLI exits non-zero. Backend errors print their typed `Display` form and also exit non-zero.

## Inspect the WIT interface

The emitted `.wasm` is a Component-Model artifact. The standalone [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools) CLI prints the WIT interface formawasm generated:

```bash
wasm-tools component wit id.wasm
```

For the `id` example above, you should see:

```wit
package formawasm:generated;

world component {
  export id: func(x: s32) -> s32;
}
```

Every `pub` formalang function appears as a WIT `export`; every `extern` function appears as an `import`. Public structs and enums become `record` and `variant` declarations on a `types` interface — see [Type Mapping](type-mapping.md).

## Run the component

formawasm doesn't ship a runtime. The standard host-side library is [`wasmtime`](https://docs.rs/wasmtime); the [Hosting a Component](hosting.md) chapter shows the full Rust-side wiring for instantiating a component, calling exports, and supplying host-provided imports.

## What's next

- Embed the backend directly in your build instead of shelling out to `formawasm` → [Using the Library](library.md).
- Understand exactly which formalang types and constructs the backend supports → [Feature Coverage](features.md).
- See the rules that govern what can cross the WIT boundary → [Boundary Policy](boundary.md).
