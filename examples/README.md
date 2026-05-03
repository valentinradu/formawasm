# Examples

End-to-end walkthroughs that compile a `.fv` source file with the `formawasm` CLI and run the resulting component under [`wasmtime`](https://wasmtime.dev) — no Rust host code involved.

## Install the toolchain

You need two binaries on your `PATH`:

```bash
# formawasm CLI (this repo)
cargo install --git https://github.com/valentinradu/formawasm formawasm

# wasmtime CLI (component-model runtime)
curl https://wasmtime.dev/install.sh -sSf | bash
# …or: cargo install --locked wasmtime-cli
# …or: brew install wasmtime    (macOS / Linux Homebrew)
```

Verify:

```bash
formawasm --help
wasmtime --version    # 22 or newer recommended for component --invoke
```

## fibonacci.fv

```bash
formawasm examples/fibonacci.fv
# wrote examples/fibonacci.wasm (… bytes) from examples/fibonacci.fv

wasmtime run --invoke 'fib(10)' examples/fibonacci.wasm
# 55
```

Try a few more:

```bash
wasmtime run --invoke 'fib(0)'  examples/fibonacci.wasm   # 0
wasmtime run --invoke 'fib(1)'  examples/fibonacci.wasm   # 1
wasmtime run --invoke 'fib(20)' examples/fibonacci.wasm   # 6765
```

## sum.fv

Three exports in one component. `--invoke` picks one by name; arguments and return type follow the WIT signature.

```bash
formawasm examples/sum.fv

wasmtime run --invoke 'double(21)'      examples/sum.wasm   # 42
wasmtime run --invoke 'sum(7, 35)'      examples/sum.wasm   # 42
wasmtime run --invoke 'factorial(5)'    examples/sum.wasm   # 120
```

You can inspect the generated WIT to see what the component exposes:

```bash
wasm-tools component wit examples/sum.wasm
```

```wit
package formawasm:generated;

world component {
  export double: func(x: s32) -> s32;
  export sum: func(a: s32, b: s32) -> s32;
  export factorial: func(n: s32) -> s32;
}
```

## What about strings, lists, records?

`wasmtime run --invoke` supports primitive parameter and return types out of the box. Components whose signatures involve `string`, `list<T>`, `record`, or `variant` cross the canonical-ABI boundary and need either:

- A small Rust host built with [`wasmtime::component::bindgen!`](https://docs.rs/wasmtime/latest/wasmtime/component/macro.bindgen.html) — see the [Hosting a Component](../docs/user/hosting.md) chapter for the full recipe.
- A higher-level runner like [`jco`](https://github.com/bytecodealliance/jco) (JavaScript host) or [`wasmtime serve`](https://docs.wasmtime.dev/cli-options.html#serve) (HTTP world).

Both options skip the per-feature host plumbing in exchange for a richer execution environment.
