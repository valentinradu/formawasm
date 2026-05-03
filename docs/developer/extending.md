# Extending the Backend

Most contributions to formawasm fall into one of three buckets: adding support for a new IR variant, lifting a feature from "pre-flight rejected" to "lowered", or wiring a new runtime helper. This page walks through each.

## Adding a new `IrExpr` variant

Suppose formalang adds `IrExpr::Bitwise { op, left, right }` for bitwise operators. The work splits across four files:

1. **`src/types.rs`** — if the new variant introduces a wasm-valtype mapping the existing `body_value_type` doesn't already cover, extend the dispatch.
2. **`src/lower/mod.rs`** — add a match arm in `lower_expr` routing the new variant to a new `lower_bitwise` function in an appropriate submodule (`binary_op.rs` if it's binary, a new file if it's a new family).
3. **`src/lower/<submodule>.rs`** — write the `lower_bitwise` function. Append wasm instructions to the caller's `InstructionSink`; do not emit a closing `end` (the function-body framer handles that).
4. **`tests/lower_bitwise.rs`** — hand-build an `IrModule` exercising the new variant and run it through `WasmBackend::generate`. Validate as a Component-Model artifact and, where possible, instantiate under wasmtime to confirm the runtime semantics.

If the new variant has a public-surface footprint (it produces a type that crosses the WIT boundary), also extend `src/wit.rs` and add an `insta` snapshot under `tests/snapshots/`.

## Lifting a feature from preflight rejection

If the IR carries something the backend currently refuses (e.g. a public closure-typed signature became expressible later), the work is:

1. **Remove the rejection in `src/preflight.rs`** for the variant in question.
2. **Implement the lowering** following the steps above.
3. **Update `tests/preflight.rs`** to drop the now-obsolete rejection case.
4. **Update [Feature Coverage](../user/features.md)** to mark the variant as supported, and add a row to [Boundary Policy](../user/boundary.md) if it crosses.

The opposite direction also happens — adding a new rejection case for a corner the backend can't actually handle. Same shape: extend `PreflightError`, add a test that exercises the rejection, document the case in [Troubleshooting](../user/troubleshooting.md).

## Wiring a new runtime helper

Runtime helpers live alongside user functions in the emitted module — there's no separate "runtime" object file. Adding one is:

1. **Declare the helper's wasm signature** as part of the module skeleton in `module_lowering::lower_module`.
2. **Emit the helper's body** as part of the helper-bootstrap section. Conventionally helpers use the `__` prefix (`__alloc`, `__str_eq`, …).
3. **Resolve the helper's function index** in the `FunctionMap` so callers can `call` it directly.
4. **Use it from a `lower_*` function** by emitting the right `call <__helper_index>` instruction sequence.

For helpers that the formalang prelude exposes through `extern impl` (e.g. `String::contains`), the wiring also touches `prelude_helper_index` so the IR-level method-call site resolves to the helper instead of an external symbol.

## Adding a new memory layout

If a new container type lands (say, `Set<T>`):

1. **Add `plan_set` to `src/layout.rs`** returning a `SetLayout` record. Follow the canonical-ABI sizing rules (1 byte `bool`, 4 bytes `s32`/`f32`, 8 bytes `s64`/`f64`, fields aligned to their own alignment, total size aligned to the max field alignment).
2. **Re-export `SetLayout` from `src/lib.rs`** so external tools can introspect it.
3. **Use the layout in lowering** — typically `lower::aggregate` for construction, `lower::reference` for access.

Layouts are planned bottom-up — every aggregate type is laid out before any function body is emitted. The lowerer resolves offsets against the layout records as compile-time constants.

## Phase milestones

Each lowering family has a milestone test that exercises the family end-to-end through the full pipeline. When you add or extend a feature, the right kind of test to add is a follow-up assertion in the relevant milestone, *or* a dedicated test under `tests/` if the feature is large enough to warrant its own file.

Existing milestones:

| Phase | Test | Coverage |
|---|---|---|
| 1a | `tests/backend_smoke.rs::fibonacci_…` | Recursive functions, primitives, `If` |
| 1b | `tests/milestone_1b.rs` | Structs, enums, methods (mut self), Match |
| 1c | `tests/sieve.rs` | Arrays, ranges, `For`, recursive helpers |
| 2 | `tests/milestone_2.rs` | Strings, Optional, Dictionary, string ops |
| 3 | `tests/milestone_3.rs` | Trait `Greet` across two impls (virtual dispatch) |
| 4 | `tests/milestone_4.rs` | Host-provided `extern fn` (component import) |

A new phase generally introduces a new milestone test. Naming follows `milestone_<phase>.rs`.

## Pattern: hand-build an IR for a test

The milestone tests don't go through the formalang frontend — they hand-build the IR using the upstream constructors so the backend gets exactly the shape under test. The pattern is:

```rust
use formalang::ir::{IrModule, IrFunction, IrFunctionParam, IrExpr, /* … */};
use formalang::ast::{PrimitiveType, ParamConvention};
use formawasm::{Backend, WasmBackend};

let mut module = IrModule::new();
module.functions.push(IrFunction {
    name: "id".to_owned(),
    generic_params: Vec::new(),
    params: vec![IrFunctionParam {
        binding_id: BindingId(0),
        name: "x".to_owned(),
        external_label: None,
        ty: Some(ResolvedType::Primitive(PrimitiveType::I32)),
        default: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }],
    return_type: Some(ResolvedType::Primitive(PrimitiveType::I32)),
    body: Some(/* IrExpr tree */),
    extern_abi: None,
    attributes: Vec::new(),
    doc: None,
    span: IrSpan::default(),
});

let bytes = WasmBackend::new().generate(&module)?;
```

This is intentional. Hand-building IR keeps the backend tests independent of the frontend's current syntax — a parser change can't break a backend test, and a backend test can exercise an IR shape the parser doesn't yet emit.
