# Crate Layout

formawasm is intentionally flat. Every top-level concern lives in its own `src/` module; only the per-IR-variant lowering family is grouped under a `lower/` subdirectory.

```text
src/
    lib.rs              # public surface re-exports
    backend.rs          # WasmBackend, Backend impl, optional wasm-opt + validation
    preflight.rs        # rejection of unsupported IR shapes
    survey.rs           # public-surface classification
    layout.rs           # memory-layout planning (struct, enum, array, range,
                        #   optional, string, dictionary, vtable)
    types.rs            # IR ResolvedType → wasm valtype mapping
    ident.rs            # source-name → kebab-case helper
    string_pool.rs      # compile-time string-literal interning
    module.rs           # core-Wasm ModuleBuilder
    module_lowering.rs  # IrModule → core wasm bytes (orchestration)
    wit.rs              # WIT auto-generation
    component.rs        # core module + WIT → component
    dwarf.rs            # (feature: dwarf) DWARF debug-section emission
    lower/              # per-IrExpr lowering (one submodule per family)
        mod.rs          # shared types + lower_expr dispatcher
        aggregate.rs    # struct/enum/tuple instantiation, field access
        binary_op.rs    # type-dispatched binary operators
        block.rs        # Block + Let + per-source scratch-local planning
        call.rs         # FunctionCall, MethodCall (static + virtual)
        control.rs      # If, For (over Range and Array)
        literal.rs      # numeric / boolean / string literals
        optional.rs     # Some-wrap coercion at let / return / args / fields
        reference.rs    # Reference, LetRef, SelfFieldRef
        unary_op.rs     # Neg, Not
    bin/
        formawasm.rs      # source.fv → output.wasm CLI driver

docs/                   # this book (mdBook source)
tests/                  # one file per IR construct + per phase milestone
plans/                  # forward-looking plan notes (decisions in flight)
```

## Module responsibilities

### `lib.rs`

Public re-exports. Anything a downstream crate imports (`use formawasm::WasmBackend`) goes through here. Also re-exports the upstream IR types (`IrModule`, `IrFunction`, `ResolvedType`) so callers don't need a separate `formalang` dependency.

### `backend.rs`

`WasmBackend` itself. Implements `formalang::pipeline::Backend`, holds the validation toggle, and orchestrates the seven-stage pipeline. The `optimize_core_module` free function lives here too — both behind the `wasm-opt` cargo feature.

### `preflight.rs`

The rejection pass. Every `PreflightError` variant carries a human-readable breadcrumb pointing at the offending IR node. Failure means an upstream pipeline step was skipped or an upstream invariant was violated.

### `survey.rs`

Classifies top-level items into exports / imports / internal types. Returns a `PublicSurface` that downstream stages consume — `lower_module` for function-index allocation, `wit::emit_wit` for which items appear in the WIT file.

### `layout.rs`

The memory-layout planner. One `plan_*` function per type family, each producing a record of per-field offsets, total size, and alignment. All sizes follow the Component-Model canonical ABI. The lowerer resolves `i32_load` / `i32_store` offsets against the constants this module returns.

### `module_lowering.rs`

The bridge between `IrModule` and `module::ModuleBuilder`. Walks the IR module, builds a `FunctionMap` ahead of time so recursive calls resolve, then lowers each function body and plugs it into a fresh `ModuleBuilder` before returning the encoded module bytes.

### `lower/`

Per-`IrExpr` lowering. Each `lower_*` function appends instructions to the caller's `InstructionSink` and assumes the surrounding stack discipline is maintained by the caller. Helpers do not emit a closing `end` — that's the function-body framer's job.

The submodules are split by IR variant family rather than by phase, so adding a new operator (e.g. a new `BinaryOp::Bitwise`) means editing exactly one file.

### `wit.rs`

WIT text generation. Walks the public surface, emits the `world` block plus any `interface types` declarations, and returns a `String` ready for `wit-component::ComponentEncoder`.

### `component.rs`

The final wrap. Takes core module bytes + WIT text, runs them through `wit-component::ComponentEncoder`, returns the component bytes.

### `string_pool.rs`

Compile-time string-literal interning. Every string literal lowered by `lower::literal` is added to the pool; on module finalization, the pool's bytes are written into a passive data segment with a single per-literal `(ptr, len)` header pair.

### `dwarf.rs`

Behind the `dwarf` cargo feature: DWARF `.debug_info` / `.debug_abbrev` / `.debug_line` / `.debug_str` custom-section construction from the `IrSpan` data formalang attaches to every IR node.

## Public-API surface

What's `pub` from this crate (per `lib.rs`):

- `WasmBackend`, `WasmBackendError` — the headline type and its error.
- `Backend` (re-exported from formalang) — the trait `WasmBackend` implements.
- `IrModule`, `IrFunction`, `ResolvedType`, `Pipeline`, etc. — re-exports so callers don't need a separate `formalang` dep.
- `lower_module`, `emit_wit`, `wrap_component` plus their error types — for callers who want to drive individual stages.
- `plan_struct`, `plan_enum`, `plan_array`, … plus their `*Layout` records — so layout-aware tools can introspect the same memory shapes the backend uses.
- `PublicSurface` — for callers who want to inspect the surface classification without re-walking the IR.

Internal helpers (`string_pool`, `ident`) stay `pub(crate)`. The boundary between "public" and "internal" is enforced at lint time: `#[deny(unreachable_pub)]` keeps the surface from leaking accidentally.

## Tests

`tests/` mirrors the lowering layout: one file per IR variant (`lower_struct_inst.rs`, `lower_array.rs`, `lower_match.rs`, …), one file per layout family (`layout_struct.rs`, `layout_array.rs`, …), plus per-phase milestone tests (`milestone_1b.rs`, `milestone_2.rs`, …, `sieve.rs`). See [Testing](testing.md) for conventions.

`tests/snapshots/` holds [`insta`](https://docs.rs/insta) snapshots of the WIT emitter output — every WIT shape we generate is captured as a `.snap` file so divergence trips a review-gated diff.
