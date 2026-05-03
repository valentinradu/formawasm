# Examples

The repo's [`examples/`](https://github.com/valentinradu/formawasm/tree/main/examples) directory has runnable `.fv` files plus a walkthrough that uses the [`wasmtime`](https://wasmtime.dev) CLI — no Rust host code required.

## Install the toolchain

```bash
# formawasm CLI
cargo install --git https://github.com/valentinradu/formawasm formawasm

# wasmtime CLI
curl https://wasmtime.dev/install.sh -sSf | bash
# …or: brew install wasmtime
```

## Compile and run

```bash
formawasm examples/fibonacci.fv
wasmtime run --invoke 'fib(10)' examples/fibonacci.wasm
# 55
```

The `examples/sum.fv` file shows multiple exports in one component:

```bash
formawasm examples/sum.fv
wasmtime run --invoke 'double(21)'   examples/sum.wasm   # 42
wasmtime run --invoke 'sum(7, 35)'   examples/sum.wasm   # 42
wasmtime run --invoke 'factorial(5)' examples/sum.wasm   # 120
```

## Limits of `wasmtime --invoke`

`--invoke` is built for primitive parameter and return types. Components whose signatures involve `string`, `list<T>`, `record`, or `variant` need a richer host:

- A Rust wrapper built with [`wasmtime::component::bindgen!`](https://docs.rs/wasmtime/latest/wasmtime/component/macro.bindgen.html) — see [Hosting a Component](hosting.md).
- [`jco`](https://github.com/bytecodealliance/jco) — JavaScript host bindings, runs in Node.js or the browser.
- [`wasmtime serve`](https://docs.wasmtime.dev/cli-options.html#serve) — HTTP-world components without per-feature glue.

The fully-walked-through commands and notes live in [`examples/README.md`](https://github.com/valentinradu/formawasm/tree/main/examples) in the repo.
