# PLAN.md — formawasm

Action-oriented plan for picking up formawasm in a fresh session. The
**[README](README.md)** is the project spec (toolchain, boundary
policy, feature → phase tables, type mapping, pipeline) — read it
first, then come back here for the "what to do now" view.

---

## Status (as of 2026-05-02 — Phase 4 complete; Phase 5+ deferred)

**Upstream — formalang at `~/projects/formalang`:**

- ✅ **PR 1 — Numeric specialization**: MERGED as `ff2a6c1`. `PrimitiveType` now has `I32 / I64 / F32 / F64`; integer-literal default is `I32`, float-literal default is `F64`; literal suffixes (`42I32`, `3.14F64`) supported.
- ✅ **PR 2 — Closure-conversion `IrPass`**: MERGED as `92fdf7c`. `ir::ClosureConversionPass` lifts every `IrExpr::Closure` to a top-level `IrFunction` paired with a synthesized capture-environment `IrStruct`, replacing the closure expression with `IrExpr::ClosureRef { funcref, env_struct, ty }`. Run between `MonomorphisePass` and `DeadCodeEliminationPass`.
- ✅ **PR — Numeric literal precision**: MERGED. `NumberLiteral.value` is now `NumberValue::{Integer(i128), Float(f64)}`; backends round-trip `i64` literals exactly.
- ✅ **PR — Resolve-references pass**: MERGED. `IrExpr::Reference` / `LetRef` / `FunctionCall` now carry typed IDs (`FunctionId`, `BindingId`, `LetId`, `FieldIdx`, `VariantIdx`, `MethodIdx`); the backend consumes them directly without re-resolving by name.

**This repo — Phase 1a is COMPLETE** (closed out 2026-04-29):

- ✅ Repo bootstrap (PR 3): scaffold + private GitHub repo + `WasmBackend` stub.
- ✅ Phase 1a mc1–mc5: deps, pre-flight checks, public-surface survey, primitive type mapping, module skeleton.
- ✅ Phase 1a mc6+: function-signature emission, every Phase 1a expression lowering (`Literal`, `Reference`, `LetRef`, `BinaryOp`, `UnaryOp`, `Block`, `Let`, direct `FunctionCall`, `If`), module-level walker (`lower_module`), WIT generation for primitive-only signatures, `wit-component` wrap, full pipeline wired through `WasmBackend::generate`. **Fibonacci milestone hit**: a recursive `fib` function compiled via the public `Backend::generate` entry point validates as a Component-Model artifact and computes correct values when instantiated under wasmtime's component runtime.

**Phase 1b is COMPLETE** (closed out 2026-04-29):

- ✅ mc1: `IrStruct` memory-layout planner.
- ✅ mc2: Bump-allocator runtime helper (`__alloc(size: i32) -> i32`).
- ✅ mc3 (split into a-d): `StructInst`, `FieldAccess`, `Tuple` lowering. Aggregates live in linear memory; the function-body planner reserves an `i32` scratch local per construction so nested allocations don't clobber each other.
- ✅ mc4: `IrEnum` memory-layout planner (uniform-size variants, `i32` discriminant tag at offset 0, payload aligned).
- ✅ mc5: `EnumInst` lowering (alloc + tag store + variant-specific field stores).
- ✅ mc6: `Match` via `br_table` on the discriminant tag, with payload-binding extraction into wasm locals.
- ✅ mc7: `SelfFieldRef` reads through wasm-local 0 (the implicit self pointer) plus `LowerContext::self_struct_id` for layout lookup.
- ✅ mc8: `MethodCall` static dispatch + impl walking. `lower_module` now walks `module.impls` and registers each method in a `MethodMap` keyed on `(ImplId, MethodIdx)`.
- ✅ mc9: `IrBlockStatement::Assign` for `SelfFieldRef` and `FieldAccess` targets (so methods with `mut self` can mutate fields).
- ✅ mc10: `ParamConvention::Sink` / `ParamConvention::Mut` confirmed to lower as `Let` for aggregates (pointer pass-through).
- ✅ mc11: `ClosureRef` materializes a `(i32 funcref, i32 env_ptr)` pair in linear memory. Indirect invocation via a funcref table is still deferred — Phase 1c+.
- ✅ mc12: WIT mapping `IrStruct` → `record`, `IrEnum` → `variant` (kebab-cased identifiers, unit and single-payload arms, multi-field variants surface as `NotYetSupported`). Round-trips through `wit_parser::Resolve`.

**Phase 1b unified milestone test:** committed as
`tests/milestone_1b.rs` — a `Counter` struct with a mutable `value`
field, an `Action` enum (`Inc`, `Add(I32)`, `Reset`), an inherent
impl with an `apply` method (Match over Action with payload binding
+ unit arms, returns a fresh Counter) and a `double` method
(mut-self via `IrBlockStatement::Assign` on `SelfFieldRef`),
exercised end-to-end through `WasmBackend::generate` and wasmtime's
component runtime.

Indirect closure invocation is the only Phase 1b mc not exercised
here; see "Known restrictions" below.

**Phase 1c is COMPLETE** (closed out 2026-04-30):

- ✅ mc1 (`da90a70`): `plan_array` layout planner — `{ ptr, len, cap }` header + per-element stride.
- ✅ mc2 (`4f15613`): `lower_array` materializes the literal end-to-end, validated under wasmtime for I32 / I64 / Boolean / struct-pointer arrays.
- ✅ mc3 (`d513052`): `Range<T>` planner + lowering for the `BinaryOperator::Range` form (`lo..hi`).
- ✅ mc4 (`c349117` + cleanup `25d6c92`): For-loop comprehension over `Range<I32>` collecting body values into a fresh `Array<body_ty>`. Loop variable threaded via the new upstream `var_binding_id` field on `IrExpr::For` (formalang `8cfe909`). Cleanup commit deduplicates array-header writes between `lower_array` / `lower_for` and replaces the silent scratch-local count with a shared constant.
- ✅ mc5 (`4be409b`): index access (`arr[i]`) reads through the array header to the element buffer.
- ✅ mc6: For over `Array<T>` lowers as a direct loop — read `in_buf` and `len` from the array header, load each element into the loop variable per iteration, store body values into a fresh `Array<body_ty>`. Implemented as a separate `lower_for_array` arm in `src/lower/control.rs` so the per-source scratch-local layout stays explicit; the seventh scratch slot (`end` in the Range path) is intentionally skipped to keep the per-For reservation count uniform with `walk_count`.
- ✅ mc7: WIT `list<T>` mapping. `resolved_wit_type` now returns owned `String`s and recurses on `ResolvedType::Array(elem)` to emit `list<inner>`. Records and function signatures pick this up for free; nested lists (`list<list<s32>>`) compose. Unsupported element types (`Never`, `Struct`, …) propagate the existing `NotYetSupported` error. Round-trips through `wit_parser::Resolve`.
- ✅ mc8: **Phase 1c milestone hit**: Sieve of Eratosthenes runs under wasmtime's component runtime. `tests/sieve.rs` hand-builds a 3-function `IrModule` (`check-divisor` self-recursive trial-division helper, `is-prime`, and `sieve(limit) -> Array<Boolean>` collecting `is-prime(p)` for `p in 0..limit`), runs it through `Pipeline::emit(module, &WasmBackend::new())`, validates the component-model artifact, instantiates under wasmtime's component runtime, and compares the returned `list<bool>` against the expected primality vector for `limit = 30`. The internal `{ ptr, len, cap }` array-header layout turns out to be canonical-ABI-compatible because `wit-component` reads only `ptr` at offset 0 and `len` at offset 4 when lifting `list<T>` returns — `cap` at offset 8 is benign extra data.

**Closed (post-Phase-1c housekeeping, 2026-04-30):**

- ✅ Indirect closure invocation. Upstream landed `IrExpr::CallClosure` (formalang `2550391`); formawasm wires a funcref `Table`, populates it with every `__closure*` lifted function, registers per-closure-type `call_indirect` signatures (env_ptr prepended), and emits `local.get base; i32_load env; <args>; i32_load funcref; call_indirect`. End-to-end coverage in `tests/lower_call_closure.rs` runs both a no-capture closure and a `make_adder` capture-closing closure under wasmtime.

**Closed (post-Phase-4 housekeeping, 2026-05-02):**

- ✅ `Range<F32>` / `Range<F64>` in `lower_for`. `range_bound_valtype` accepts the four numeric primitives; setup narrows `(end - start)` to i32 via `f32.ceil` / `f64.ceil` followed by `i32_trunc_sat` (so the output buffer is large enough to hold every iteration when the gap is fractional); the per-iteration buffer-offset compute uses `i32_trunc_sat` on the float counter. Iteration semantics: advance by `1.0` each step. End-to-end coverage in `tests/lower_for.rs::for_over_f32_range_iterates_one_per_step` (squares of `0.0..3.0` → `[0.0, 1.0, 4.0]`) and `for_over_f64_range_iterates_one_per_step` (`x + x` over `1.0..4.0` → `[2.0, 4.0, 6.0]`).

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

**Phase 2 is COMPLETE** (closed out 2026-05-01):

- ✅ mc1 (`5a0ec1b`): `plan_optional` layout planner — uniform
  `{ tag: i32, payload: T }` record; `Optional<Never>` is the static
  type of the `nil` literal and lays out as a tag-only 4-byte cell;
  aggregate inner types collapse to a 4-byte pointer payload.
- ✅ mc2 (`c1d113f`): `Literal::Nil` materializes a tag-only Optional
  in linear memory (alloc 4 bytes, store `OPTIONAL_TAG_NIL` at offset
  0, push pointer). `body_value_type` now maps `Optional<T>` to `i32`
  so let-bindings / params / returns travel as pointers — same
  convention every other aggregate uses.
- ✅ mc3 (`4f6d1f5`): WIT mapping `Optional<T>` → `option<inner>`.
  Symmetric with the `Array<T>` → `list<inner>` recursion;
  `Optional<Never>` stays rejected because WIT has no zero-payload
  `option<>` form. Round-trips through `wit_parser::Resolve`.
- ✅ mc4 (`80edbf6`): Some-wrap let-binding values into Optional<T>.
  New `lower::optional` module owns the wrap helper plus the
  `some_wrap_payload` predicate let / walk_count consult. Per-site
  scratch reservation: 1 i32 (cell ptr) + 1 typed slot matching
  payload's wasm value type. I32 / I64 / Boolean payload coverage.
- ✅ mc5 (`8195e4c`): Coerce Optional<T> at function returns, if
  branches, match arms. `lower_function_body_in_module` now takes
  the return type and threads it to `count_scratch_locals` /
  `finish_function_body`. New `lower_coerced` + `coercion_scratch_counts`
  helpers funnel every site through one path. `if`'s block-type
  derivation switches to `body_value_type` so `Optional<T>` returns
  map to a single i32 result instead of failing as `NotYetSupported`.
- ✅ mc6 (`f4c7c6a`): Coerce Optional<T> at function and method call
  argument sites. `walk_count` plumbs `module: Option<&IrModule>`
  through so per-site target-type lookups (callee parameter types,
  struct/enum field declared types) fire alongside the regular walk.
- ✅ mc7 (`e590eef`): Coerce Optional<T> at struct fields, tuple
  slots, array elements. Layout planner accepts `Optional<T>` as a
  4-byte pointer field/element anywhere a slot is allowed; new
  `store_aggregate_field` helper picks the right store opcode
  (i32_store for Optional, primitive store for primitives). The
  `OptionalField` LayoutError variant is gone — the `optional: bool`
  AST flag and resolved `Optional(T)` type now coexist without forcing
  a rejection.
- ✅ mc8 (`e60c020`): `plan_string` returns the canonical
  `{ ptr: i32, len: i32 }` 8-byte / 4-aligned header. Strings are
  immutable so there is no `cap` slot.
- ✅ mc9 (`e2d57ce`): `Literal::String` seeds bytes + header into the
  wasm `data` section. New `StringPool` interns every literal during
  a pre-walk; `ModuleBuilder::finish` emits the active data segment
  and bumps `HEAP_BASE` past it. `body_value_type` maps String to
  i32 (header pointer); the literal lowers to a single `i32.const
  <header_offset>`.
- ✅ mc10 (`35addd6`): `BinaryOp::Eq / Ne` on String operands routes
  through a `__str_eq` runtime helper. `Eq` calls it directly; `Ne`
  follows up with `i32.eqz`.
- ✅ mc11 (`f20aec0`): `BinaryOp::Add` on String operands routes
  through `__str_concat` — allocates buffer + header,
  `memory.copy`s both inputs in, returns the new header pointer.
- ✅ mc12 (`69c4290`): WIT mapping `String` / `Path` / `Regex` →
  `string` plus a canonical-ABI export wrapper that splits each
  string / list parameter into the host's expected `(ptr, len)`
  pair. `cabi_realloc` is exported so the runtime can allocate
  inbound buffers in our linear memory. End-to-end roundtrip
  through wasmtime's component runtime.
- ✅ mc13 (`a4c1b99`): `Dictionary<K, V>` v1 — literal construction
  + linear-scan lookup. v1 layout is `{ ptr, len, cap }` plus a
  buffer of pointers, each pointing to a `(k, v)` pair tuple. String
  keys compare via `__str_eq`; missing keys trap. Layout planner
  accepts String / Path / Regex as 4-byte pointer-sized fields,
  optional payloads, and range bounds.
- ✅ mc14 (`1f76f6d`): WIT mapping `Dictionary<K, V>` →
  `list<tuple<K, V>>` (component-model tuple type at the boundary).
- ✅ Phase 2 milestone (`9aa9ef2`): `tests/milestone_2.rs` —
  `greet(role: String) -> String` with a `Dictionary<String, String>`
  literal lookup, an `I32?` Some-wrap site, and chained string
  concatenation. Round-trips three roles through wasmtime's
  component runtime.

**Phase 3 is COMPLETE** (closed out 2026-05-02):

- ✅ mc1: `plan_vtable` layout planner — `methods * 4` flat array of
  `i32` funcref-table indices, 4-aligned. Empty / single-method /
  multi-method traits all produce the same uniform shape.
- ✅ mc2: Per-impl vtable plumbing. `build_vtable_plumbing` declares
  a fresh funcref `Table` sized to total impl-method count, populates
  it with each trait-impl method's wasm function index, then walks
  every `impl Trait for Type` and seeds vtable bytes
  (`funcref_slot.to_le_bytes()`, one slot per trait method, ordered
  to match `IrTrait.methods`) into the static-data segment after the
  string-pool region. `(TraitId, ImplTargetKey) → vtable_offset`
  and `(TraitId, MethodIdx) → call_indirect_type_index` flow into
  per-function lowering through a new `VTableContext`.
- ✅ mc3: `DispatchKind::Virtual` lowering. Resolves the receiver's
  concrete type at compile time (`Struct` / `Enum`), looks up the
  vtable's absolute byte offset, emits
  `i32.const 0; i32.load offset=vtable_base+method_idx*4` to read
  the funcref-table slot, pushes receiver + Optional-coerced args,
  emits `call_indirect <method_table_idx>, <type_idx>`. Static
  dispatch keeps the existing `call <wasm_idx>` path. Optional
  argument coercion now consults the trait method's signature for
  Virtual dispatch (matches what static dispatch reads from the
  impl block).
- ✅ Phase 3 milestone (`tests/milestone_3.rs`): one trait `Greet`
  with method `value(self) -> I32`, two struct impls returning 1 / 2
  respectively, two top-level `dispatch_alpha` / `dispatch_beta`
  functions invoking `.value()` via `DispatchKind::Virtual`.
  Runs through `Pipeline::new()` (no `MonomorphisePass`, so the
  Virtual call sites survive into the backend), validates the
  component-model artifact, instantiates under wasmtime's component
  runtime, and confirms each function dispatches to its own impl.

**Phase 4 is COMPLETE** (closed out 2026-05-02):

- ✅ mc1 (`b9d839f`): WIT-side `import` emission. Every
  `extern_abi`-bearing function in `surface.imports` lands in the
  world block as `import <kebab-name>: func(...) -> ...;`. Round-
  trips through `wit_parser::Resolve`.
- ✅ mc2 (`6f3a0ec`): Core-wasm import section. ModuleBuilder grows
  `declare_function_import` and an `ImportSection`. Imports occupy
  the leading region of the wasm function-index space; runtime
  helpers and locally-defined user functions ladder up behind them.
  Imports lift under module name `cm32p2` (wit-component's
  canonical-ABI mangling). The old `ExternFunction` reject-on-emit
  path is gone — extern functions are first-class now;
  `MissingFunctionBody` survives as a body-None invariant check
  for malformed non-extern functions.
- ✅ Phase 4 milestone (`tests/milestone_4.rs`): one extern
  `host_double(n: I32) -> I32` declaration + one local
  `call_host(n: I32) -> I32 { host_double(n) }`. Runs through
  `Pipeline::new()` + `WasmBackend::new()`, validates the
  component-model artifact, instantiates under wasmtime with a
  `Linker` that maps `host-double` to `|n| 2 * n`, calls
  `call_host(21)` and confirms the result is 42.

**Known restrictions carried forward from Phase 4:**

- `ResolvedType::External` cross-module references stay rejected
  by the layout planner / type mapper / lowering. The IR shape
  exists (a `use other_module::Helper;` produces references
  carrying `External { module_path, name, kind, type_args }`), but
  the language has no `use`-syntax driver reaching the backend
  yet, so the path is dead code today. Nested `module.modules`
  are still not walked either. Both lift cleanly once a real
  consumer exercises them.

After Phase 4 closes, Phase 5+ picks up post-pass optimization
(`wasm-opt`), DWARF debug info, GC, and async — all separate
initiatives per the README roadmap.

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
