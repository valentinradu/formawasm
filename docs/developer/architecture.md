# Architecture

formawasm is a single-crate compiler backend. Its job is to turn one `IrModule` into one `Vec<u8>` — wrapped Component-Model bytes — and surface every error as a typed value. This page traces the journey from input IR to output bytes.

## The pipeline

`WasmBackend::generate(&module)` runs seven stages, in order:

```text
preflight ──► survey ──► lower_module ──► [wasm-opt] ──►
            emit_wit ──► wrap_component ──► [validate]
```

Each stage lives in its own source module and surfaces a typed error.

| Stage | Module | Job |
|---|---|---|
| `preflight::check` | `src/preflight.rs` | Reject leftover `IrExpr::Closure`, public closure-typed signatures, generic traits, `ResolvedType::TypeParam`, `ResolvedType::Error`. Fail fast. |
| `survey::survey` | `src/survey.rs` | Walk `IrModule`; classify every top-level item as export / import / internal. Returns a `PublicSurface`. |
| `module_lowering::lower_module` | `src/module_lowering.rs` | Plan memory layouts, declare runtime helpers, declare extern imports under `cm32p2`, declare funcref tables for closures + trait methods, build per-function bodies, concatenate static-data segments. Returns core wasm bytes. |
| `optimize_core_module` (optional) | `src/backend.rs` | Behind the `wasm-opt` cargo feature: run binaryen at `-Os` over the core bytes with `Feature::All` enabled. |
| `wit::emit_wit` | `src/wit.rs` | Walk the public surface; emit `import` / `export` lines plus `record` / `variant` declarations for public structs / enums. |
| `component::wrap_component` | `src/component.rs` | Feed core module bytes + WIT to `wit-component::ComponentEncoder`. Returns wrapped component bytes. |
| `validate_component` (optional) | `src/backend.rs` | When constructed via `with_validation`: run `wasmparser::Validator` against the wrapped bytes. |

## Why this shape

A few decisions are worth calling out, because they constrain how new features compose:

**Preflight is a separate stage, not interleaved.** A bad IR shape should fail before we do any work — the lowering paths can then assume well-formed input and skip defensive checks.

**The survey runs before lowering.** Knowing the export and import sets upfront lets the lowerer commit to function-index allocations early, so it never has to reorder or renumber.

**Layouts are planned bottom-up, lowering top-down.** The layout planner (`src/layout.rs`) computes one record per type before any function body is emitted; the lowerer then resolves `i32_load` / `i32_store` offsets against compile-time constants. This is what makes per-method dispatch a direct index into a vtable instead of a runtime map lookup.

**Validation is opt-in.** `wit-component` already validates internally during wrap, and a defensive `wasmparser` re-check on the hot path adds cost no production user pays. Tests that construct backends via `with_validation()` get the safety net; the default builder skips it.

**The optimizer pass runs on core wasm, not on the wrapped component.** Binaryen's component-model support is still young, and the canonical-ABI wrappers / `cabi_realloc` export are easier to keep intact when wrapping happens after.

## Boundary representation

The backend has two representational regimes:

- **Inside the core module**: full power of core wasm. Linear memory for aggregates, tables for funcrefs, multi-value returns where they help. Lowering is free to use any wasm proposal we've enabled.
- **At the WIT boundary**: the canonical ABI. Aggregates flow as `(ptr, len)` pairs or pointers into linear memory; the `cabi_realloc` export gives the host a hook into the component's allocator.

Translating between the two regimes happens in two places: WIT-generated parameter-split wrappers (lift inbound boundary values to internal pointers) and return-shape wrappers (lower outbound internal pointers to canonical-ABI return values). Both are emitted by `lower_module` alongside user functions.

## Per-module compile

The `lower_module` stage is the heavy lifter. Inside a single call:

1. **Plan layouts** for every aggregate type (`plan_struct`, `plan_enum`, `plan_array`, `plan_range`, `plan_optional`, `plan_string`, `plan_dictionary`, `plan_vtable`).
2. **Declare runtime helpers**: bump allocator (`__alloc`), string equality (`__str_eq`), string concatenation (`__str_concat`), canonical-ABI realloc (`cabi_realloc`).
3. **Declare extern imports** under the `cm32p2` namespace (canonical-ABI 32-bit-platform-2 mangling per `wit-component`). Imports occupy the leading region of the function-index space.
4. **Declare funcref tables**: one for closures, one per trait for vtable dispatch.
5. **Lower each function body** via the `src/lower/*` submodules; each `lower_*` function appends instructions to the caller's `InstructionSink`.
6. **Concatenate static data**: string-pool bytes + per-impl vtables get written into a single passive data segment seeded into linear memory at startup.
7. **Emit the wasm `name` custom section** so debug tooling resolves `func[N]` back to the source identifier.

The resulting `Vec<u8>` is a fully-formed core wasm module, ready for `wit-component` to wrap.

## Where the IR comes from

formawasm doesn't parse `.fv` source files itself. The expected pre-codegen pipeline is:

```rust
Pipeline::new()
    .pass(MonomorphisePass::default())          // specialize generics
    .pass(ResolveReferencesPass::new())         // stamp typed IDs
    .pass(ClosureConversionPass::new())         // lift closures
    .pass(DeadCodeEliminationPass::new())       // strip unreachable
    .emit(module, &WasmBackend::new())
```

Skipping `MonomorphisePass` leaves `ResolvedType::Generic` / `TypeParam` in the IR, which preflight rejects. Skipping `ClosureConversionPass` leaves `IrExpr::Closure` in the IR, which preflight also rejects. The other two passes are quality-of-life rather than correctness — `ResolveReferencesPass` lets the backend skip name resolution at lowering time, and `DeadCodeEliminationPass` keeps the emitted bytes small.
