# Using the Library

`formawasm` exposes a single backend type — `WasmBackend` — that implements formalang's `Backend` trait. The same backend is what `formawasm CLI` drives internally; using it from your own crate gives you control over which IR passes run, how diagnostics are reported, and where the bytes go.

## Add the dependency

In your `Cargo.toml`:

```toml
[dependencies]
formalang = "0.0.2-beta"
formawasm = { git = "https://github.com/valentinradu/formawasm" }
```

(formawasm is pre-1.0 and not on crates.io yet; pin to a commit if you need stable upstream behavior.)

## End-to-end example

```rust
use formalang::{
    FileSystemResolver, Pipeline, compile_to_ir_with_resolver,
    ir::{ClosureConversionPass, DeadCodeEliminationPass,
         MonomorphisePass, ResolveReferencesPass},
};
use formawasm::WasmBackend;
use std::path::PathBuf;

fn build_component(source_path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(source_path)?;

    // Resolve `use foo::bar` against the source file's parent directory.
    let resolver = FileSystemResolver::new(
        PathBuf::from(source_path).parent().unwrap_or(".".as_ref()).to_path_buf(),
    );
    let module = compile_to_ir_with_resolver(&source, resolver)
        .map_err(|errors| format!("{} compile errors", errors.len()))?;

    // Standard codegen pipeline. Order matters: monomorphise specializes
    // generics, resolve-references stamps typed IDs, closure-conversion
    // lifts every closure to a top-level function, DCE strips dead code.
    let mut pipeline = Pipeline::new()
        .pass(MonomorphisePass::default())
        .pass(ResolveReferencesPass::new())
        .pass(ClosureConversionPass::new())
        .pass(DeadCodeEliminationPass::new());

    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    Ok(bytes)
}
```

The four passes shown are the canonical pre-codegen sequence: skipping any of them violates an invariant the backend relies on (see [Architecture](../developer/architecture.md)). The CLI runs the same sequence.

## The `Backend` trait

`WasmBackend` implements [`formalang::pipeline::Backend`]:

```rust
pub trait Backend {
    type Output;
    type Error;
    fn generate(&self, module: &IrModule) -> Result<Self::Output, Self::Error>;
}
```

For `WasmBackend`:

- `Output = Vec<u8>` — the wrapped component bytes.
- `Error = WasmBackendError` — a typed enum covering preflight failures, lowering errors, WIT emission, component wrapping, and (when enabled) the optional `wasm-opt` and validation steps.

You can also call `WasmBackend::new().generate(&module)` directly if you've built the IR by hand or don't need the pass infrastructure.

## Optional steps

`WasmBackend` is configured with a builder-style API:

```rust
use formawasm::WasmBackend;

// Re-validate the wrapped component bytes through `wasmparser`
// before returning. Off by default; surfaces backend bugs as
// `WasmBackendError::Validation` instead of as runtime failures
// inside the embedding host.
let backend = WasmBackend::new().with_validation();
```

Build-time options live behind cargo features — see [Cargo Features](cargo-features.md).

## Observability

The backend is instrumented with [`tracing`](https://docs.rs/tracing). The default no-subscriber path is essentially free; install a subscriber if you want to see per-stage timings and byte counts:

```rust
use tracing_subscriber::EnvFilter;

tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

Then run with `RUST_LOG=formawasm=debug` to see entries for `preflight`, `survey`, `lower_module`, `emit_wit`, and `wrap_component`, with byte sizes attached to each stage.

## Re-exports

`formawasm` re-exports the formalang IR types it consumes, so callers don't need a separate `formalang` dependency for the type surface:

```rust
pub use formalang::ir::{IrModule, IrFunction, ResolvedType, /* … */};
pub use formalang::pipeline::{Backend, Pipeline, IrPass};
```

You only need a direct `formalang` dependency if you're calling the parser (`compile_to_ir`, `compile_to_ir_with_resolver`) or constructing IR via the upstream constructors.
