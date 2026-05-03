# Cargo Features

formawasm has two optional cargo features. Both are off by default; enabling them only adds dependencies (and compile cost) for callers that opt in.

## `wasm-opt`

Runs [binaryen](https://github.com/WebAssembly/binaryen)'s `wasm-opt` over the emitted core module before component wrapping.

Enable in your `Cargo.toml`:

```toml
[dependencies]
formawasm = { version = "...", features = ["wasm-opt"] }
```

Or, when compiling formawasm directly:

```bash
cargo build --features wasm-opt
make test-wasm-opt
```

When the feature is on, `WasmBackend::generate` invokes the optimizer at `-Os` (size-leaning) with `Feature::All` enabled, so multi-table, reference types, and bulk-memory survive the pass. Output is typically 30–50% smaller than the unoptimized core module on real workloads.

The pass is also exposed as a free function — `formawasm::optimize_core_module(&bytes)` — for callers that obtain core wasm by other means and want to run the same post-pass without going through `WasmBackend`.

The `wasm-opt` crate compiles binaryen from source on first build, which adds several minutes to a clean compile. Production users typically gate this behind a release-only profile.

## `dwarf`

Attaches DWARF debug sections (`.debug_info`, `.debug_abbrev`, `.debug_line`, `.debug_str`) to the emitted core module so source-level debuggers can map wasm addresses back to formalang source lines.

```toml
[dependencies]
formawasm = { version = "...", features = ["dwarf"] }
```

Granularity is function-level: one subprogram DIE per user function with `name + decl_file + decl_line + low_pc / high_pc`, plus a `.debug_line` row pointing at each function's first source line. Per-statement line tables can layer on later.

The feature pulls in [`gimli`](https://docs.rs/gimli) and the IR-side `IrSpan` data formalang attaches to every node. Default-feature builds skip the dependency entirely.

## `with_validation()` (always available)

Not a cargo feature, but worth mentioning here: `WasmBackend::new().with_validation()` enables a `wasmparser::Validator` re-check against the wrapped component bytes before returning. Surfaces internal-error conditions (malformed core wasm slipping through, canonical-ABI mismatches) as `WasmBackendError::Validation` instead of as runtime failures inside the embedding host.

The pass adds one validation pass per `generate` call; off by default for speed, on for production correctness when the cost is acceptable.

## Combining features

All three are independent and compose cleanly:

```toml
[dependencies]
formawasm = { version = "...", features = ["wasm-opt", "dwarf"] }
```

Order of operations in `WasmBackend::generate` when all are enabled:

```text
preflight ──► survey ──► lower_module ──► [wasm-opt post-pass]
            ──► emit_wit ──► wrap_component ──► [validation]
```

DWARF sections are emitted by `lower_module`, so they ride through the `wasm-opt` pass intact (binaryen preserves custom sections by default).
