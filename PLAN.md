# PLAN.md — formawasm

Action-oriented plan for picking up formawasm in a fresh session. The
**[README](README.md)** is the project spec (toolchain, boundary
policy, feature → phase tables, type mapping, pipeline) — read it
first, then come back here for the "what to do now" view.

---

## Status (as of 2026-04-29)

**Upstream — formalang at `~/projects/formalang`:**

- ✅ **PR 1 — Numeric specialization**: MERGED as `ff2a6c1`. `PrimitiveType` now has `I32 / I64 / F32 / F64`; integer-literal default is `I32`, float-literal default is `F64`; literal suffixes (`42I32`, `3.14F64`) supported.
- ✅ **PR 2 — Closure-conversion `IrPass`**: MERGED as `92fdf7c`. `ir::ClosureConversionPass` lifts every `IrExpr::Closure` to a top-level `IrFunction` paired with a synthesized capture-environment `IrStruct`, replacing the closure expression with `IrExpr::ClosureRef { funcref, env_struct, ty }`. Run between `MonomorphisePass` and `DeadCodeEliminationPass`.
- ✅ **PR — Numeric literal precision**: MERGED. `NumberLiteral.value` is now `NumberValue::{Integer(i128), Float(f64)}`; backends round-trip `i64` literals exactly.
- ✅ **PR — Resolve-references pass**: MERGED. `IrExpr::Reference` / `LetRef` / `FunctionCall` now carry typed IDs (`FunctionId`, `BindingId`, `LetId`, `FieldIdx`, `VariantIdx`, `MethodIdx`); the backend consumes them directly without re-resolving by name.

**This repo — Phase 1a is COMPLETE** (closed out 2026-04-29):

- ✅ Repo bootstrap (PR 3): scaffold + private GitHub repo + `WasmBackend` stub.
- ✅ Phase 1a mc1–mc5: deps, pre-flight checks, public-surface survey, primitive type mapping, module skeleton.
- ✅ Phase 1a mc6+: function-signature emission, every Phase 1a expression lowering (`Literal`, `Reference`, `LetRef`, `BinaryOp`, `UnaryOp`, `Block`, `Let`, direct `FunctionCall`, `If`), module-level walker (`lower_module`), WIT generation for primitive-only signatures, `wit-component` wrap, full pipeline wired through `WasmBackend::generate`. **Fibonacci milestone hit**: a recursive `fib` function compiled via the public `Backend::generate` entry point validates as a Component-Model artifact and computes correct values when instantiated under wasmtime's component runtime.

- ⏳ **Phase 1b in progress**: aggregates, methods, calling conventions. See README's "Roadmap" for the full microcommit list; the immediate-work section below tracks the current mc.

> **Quality bar.** This repo mirrors the lint / CI / build setup at
> `~/projects/smid/smid-ws0` — strict clippy (deny `unwrap_used`,
> `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`,
> `print_*`, `dbg_macro`, `arithmetic_side_effects`, lossy casts,
> etc.), rustfmt-checked CI, `cargo deny` gates, tests return
> `Result` (no bare `assert!` / `panic!`), async where genuinely
> needed. Errors are always typed (`thiserror`) and propagated.
> When the existing PLAN/README text below references formalang's
> setup, prefer smid-ws0's instead.

---

## What you're building

formalang produces a typed `IrModule`; formawasm walks it and emits a
WebAssembly **component** (not a core module — Component Model from
day one). The host runtime is whatever the caller picks (wasmtime
primary, wasmi for pure-Rust embed, browsers via `jco` transpile).

Every public formalang declaration becomes a typed entry point in
the component's WIT-described interface. The full formalang language
is supported *inside* the component; the WIT-expressible subset
applies only to types in `pub` signatures (see README's "Boundary
policy").

---

## The formalang IR you will consume

When PR 3 mc2 adds the `formalang = { path = "../formalang" }` dep,
these are the types you'll touch most:

| Type | Path | Notes |
|---|---|---|
| `IrModule` | `formalang::ir::IrModule` | Root; has `structs`, `traits`, `enums`, `impls`, `lets`, `functions`, `imports`, `modules`. Indices rebuilt by `rebuild_indices()` after structural changes. |
| `IrExpr` | `formalang::ir::IrExpr` | Expression tree. Every variant carries `ty: ResolvedType`. |
| `IrExpr::ClosureRef { funcref, env_struct, ty }` | same | The post-conversion form of a closure value. `funcref` names a top-level `__closure<N>` function; `env_struct` is the env-construction expression. |
| `IrStruct` / `IrEnum` / `IrFunction` | `formalang::ir::types` | Definitions. `IrField.convention: ParamConvention` distinguishes Let / Mut / Sink semantics on env-struct fields. |
| `ResolvedType` | `formalang::ir::ResolvedType` | Type system. Look at `Closure { param_tys, return_ty }` for closure types. |
| `ParamConvention` | `formalang::ast::ParamConvention` | `Let / Mut / Sink`. |
| `Backend` / `IrPass` / `Pipeline` | `formalang::pipeline` | The trait you'll implement is `Backend`. |
| `MonomorphisePass`, `ClosureConversionPass`, `DeadCodeEliminationPass` | `formalang::ir` | Run them via `Pipeline` before `WasmBackend::generate`. |

The IR is mature and stable now — both PRs that were "in active flux"
landed. You can rely on the shapes above.

---

## The pipeline you'll plug into

Standard usage from a host:

```rust
use formalang::{compile_to_ir, Pipeline};
use formalang::ir::{MonomorphisePass, ClosureConversionPass, DeadCodeEliminationPass};
use formawasm::WasmBackend;

let module = compile_to_ir(&source)?;
let bytes = Pipeline::new()
    .pass(MonomorphisePass::default())
    .pass(ClosureConversionPass::new())
    .pass(DeadCodeEliminationPass::new())
    .emit(module, &WasmBackend::new())?;
```

After `ClosureConversionPass` runs, the IR is closure-free —
`WasmBackend` only needs to handle `IrExpr::ClosureRef`, never
`IrExpr::Closure`. Pre-flight checks should verify this and reject
the residual case (it'd indicate the caller forgot to run the pass).

---

## Immediate work — Phase 1b mc1: `IrStruct` memory-layout planner

The first 1b mc lands a pure-data-structure pass that decides where
every `IrStruct` field lives in linear memory. Nothing emits code yet
— this just produces the layout information later mcs will consume
(bump-allocator, `StructInst`, `FieldAccess`, `Tuple`, env-struct
materialization for `ClosureRef`).

**Scope**

- New module `src/layout.rs` (or `src/struct_layout.rs`).
- `pub struct StructLayout { pub size: u32, pub align: u32, pub fields: Vec<FieldLayout> }`.
- `pub struct FieldLayout { pub offset: u32, pub size: u32, pub align: u32 }`.
- `pub fn plan_struct(s: &IrStruct, module: &IrModule) -> Result<StructLayout, LayoutError>`.
- ABI: follow Component-Model canonical ABI sizes/aligns for primitives — `bool` = 1/1, `s32`/`f32` = 4/4, `s64`/`f64` = 8/8. Each field starts at the next offset rounded up to its alignment; total size is rounded up to the struct's alignment (max of field alignments, min 1).
- For Phase 1b mc1 the supported field types are *primitives only*; nested structs / enums / arrays / tuples surface as `LayoutError::NotYetSupported`. Later mcs extend coverage as their lowerings land.

**Tests** (in `tests/layout.rs`, smid-style `TestResult`)

- empty struct → size 0, align 1.
- single `s32` field → size 4, align 4, offset 0.
- two `s32` fields → size 8, align 4, offsets 0 and 4.
- `s32, s64` → size 16, align 8, offsets 0 and 8 (4-byte gap to align the i64).
- `bool, s32` → size 8, align 4, offsets 0 and 4 (3-byte gap).
- `bool, bool, s32` → size 8, align 4, offsets 0, 1, 4.
- `s64, s32` → size 16, align 8, offsets 0 and 8 (trailing pad to 16).
- nested struct field → `LayoutError::NotYetSupported`.

**Verify**: `cargo build` + `cargo clippy --all-targets -- -D warnings`
+ `cargo test` green.

After mc1 lands, the next mc emits a bump-allocator runtime helper
(`__alloc(size: i32) -> i32` over the heap-pointer global) so that
`StructInst` lowering in the following mc can call it.

---

## Phase 1 microcommit list (~30 commits to v0.1)

Three sub-phases, each ending in a wasmtime end-to-end milestone.
See README's "Roadmap" section for the full microcommit list.

| Sub-phase | Milestone | Scope |
|---|---|---|
| **1a** | fibonacci runs under wasmtime | Primitives, control flow (`If`, `Block`), direct `FunctionCall`, `Let`/`LetRef`, `BinaryOp`/`UnaryOp` (numeric/boolean/comparison), pre-flight checks, module skeleton, WIT for primitives, component wrap |
| **1b** | struct + enum + methods + mutable params + intramodule closure | `IrStruct` / `IrEnum` layout, `Match` via `br_table`, `MethodCall` (Static), `SelfFieldRef`, `Tuple`, `ParamConvention::Mut` / `Sink`, `ClosureRef` lowering, WIT records + variants |
| **1c** | sieve-of-Eratosthenes | `Array<T>`, `Range<T>`, `For` over both, WIT lists |

---

## After Phase 1

- **Phase 2** (~14 commits): strings, optionals, dictionaries.
- **Phase 3** (4 commits): virtual dispatch (`DispatchKind::Virtual`).
- **Phase 4** (~6 commits): `extern_abi` imports, `ResolvedType::External` cross-module references.
- **Phase 5+**: `wasm-opt`, DWARF, GC, async — deferred, separate initiatives.

---

## Pre-flight checks (already implemented)

These rejections live in `src/preflight.rs` and run as the first
step of `WasmBackend::generate`.

Reject the module with a typed error if any of these hold:

1. Any `IrExpr::Closure` survives. Means `ClosureConversionPass`
   wasn't run; the post-conversion `IrExpr::ClosureRef` is what
   we handle. (`ClosureConversionPass` itself enforces this
   invariant on its own output, but pre-flight defense is cheap.)
2. Any public (`pub`) function or struct field has a closure-typed
   signature. Closures cannot cross the WIT boundary; this would
   surface only at WIT generation if not caught early.
3. Any generic trait survives in `module.traits` (i.e.
   `!t.generic_params.is_empty()`). Matches the existing
   `MonomorphisePass` constraint.
4. Any `ResolvedType::TypeParam` reaches us — must already be
   monomorphised.
5. Any `ResolvedType::Error` placeholder. Indicates a frontend bug;
   surface explicitly rather than emit broken Wasm.

Test each rejection with a hand-built `IrModule` containing the
offending shape and assert the error variant.

---

## Open questions to revisit during Phase 1

- **`IrEnum` payload packing**: uniform-size variants (simpler, more
  memory) vs minimal-size with offset table (compact, more code).
  Decide before the `IrEnum` layout-planner mc.
- **Stack vs heap for aggregates**: small structs can live in Wasm
  locals; large ones must be bump-allocated. Pick a size threshold
  during the `StructInst` lowering mc — the mc1 layout planner is
  agnostic about it.
- **Default parameter values**: lower as wrapper functions or
  expand at the call site? Decide before any function with
  defaults reaches the test corpus.
- **String operations beyond concat / equality**: ship `len` /
  `slice` / formatting in-module, or require them via `extern_abi`?
  Phase 2 question.

---

## Resuming this work

A fresh Claude session in `~/projects/formawasm` should:

1. Read `README.md` for the spec, then this `PLAN.md` for what's
   next.
2. Skim `git log --oneline` to see how far past the documented
   immediate-work mc the tree has actually moved.
3. Verify formalang's IR shape hasn't shifted by reading
   `~/projects/formalang/src/ir/expr.rs` and
   `~/projects/formalang/src/ir/mod.rs` for the latest variant
   shapes.
4. Pick up the next un-committed mc — start from the
   "Immediate work" section above if the tree matches the documented
   state, or read the README's roadmap for items past that point.

Microcommit cadence: one commit per microcommit, descriptive message,
verify `cargo build` + `cargo clippy --all-targets -- -D warnings` +
`cargo test` green before each commit. Mirror smid-ws0's commit
style (subject in `<scope>: <verb>` form, body explaining *why*).
