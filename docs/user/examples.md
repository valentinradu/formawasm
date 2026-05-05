# Examples

The repo's [`examples/`](https://github.com/valentinradu/formawasm/tree/main/examples) directory has runnable `.fv` files plus a walkthrough that uses the [`wasmtime`](https://wasmtime.dev) CLI — no Rust host code required.

## Install the toolchain

```bash
# formawasm CLI
cargo install formawasm

# wasmtime CLI
curl https://wasmtime.dev/install.sh -sSf | bash
# Or: brew install wasmtime
```

## Compile and run

The repo's numbered `.fv` files each declare one or more `pub fn`
exports plus a `pub fn run_checks()` that asserts the expected
outputs. Compile any of them with the CLI:

```bash
formawasm examples/02_generics_pair_result.fv
wasmtime run --invoke 'pair-sum()' examples/02_generics_pair_result.wasm
# 3
```

Examples like `12_numeric_primitives.fv` carry several primitive-typed
exports in one component:

```bash
formawasm examples/12_numeric_primitives.fv
wasmtime run --invoke 'i32-add(7, 35)'  examples/12_numeric_primitives.wasm   # 42
wasmtime run --invoke 'i64-mul(6, 7)'   examples/12_numeric_primitives.wasm   # 42
```

To exercise the embedded `assert(...)` calls in each example's
`run-checks` export, run the test harness — it wires `assert` to
a host function that traps on `false`, so passing means every
embedded assertion held:

```bash
cargo test --test examples
```

## Limits of `wasmtime --invoke`

`--invoke` is built for primitive parameter and return types. Components whose signatures involve `string`, `list<T>`, `record`, or `variant` need a richer host:

- A Rust wrapper built with [`wasmtime::component::bindgen!`](https://docs.rs/wasmtime/latest/wasmtime/component/macro.bindgen.html) — see [Hosting a Component](hosting.md).
- [`jco`](https://github.com/bytecodealliance/jco) — JavaScript host bindings, runs in Node.js or the browser.
- [`wasmtime serve`](https://docs.wasmtime.dev/cli-options.html#serve) — HTTP-world components without per-feature glue.

The fully-walked-through commands and notes live in [`examples/README.md`](https://github.com/valentinradu/formawasm/tree/main/examples) in the repo.
