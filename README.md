# formawasm

formawasm is a compiler that turns a formalang [Intermediate Representation (IR)](https://github.com/RadValentin/formalang) module into a [WebAssembly](https://webassembly.org) **component** — a `.wasm` binary that any standards-compliant WebAssembly runtime can execute. The host application picks the runtime ([wasmtime](https://wasmtime.dev), [wasmi](https://github.com/wasmi-labs/wasmi), a browser engine, …); formawasm only emits bytes.

```text
formalang frontend ──► IrModule ──► formawasm ──► .wasm component ──► host runtime
```

Every public formalang declaration becomes a typed entry point in the component's interface. Inside the component, the full formalang language is supported — see the [Feature coverage](#feature-coverage) section for the exact mapping from IR variants to compile phases.

---

## What is WIT?

**WIT** stands for **Wasm Interface Types**. It is the Interface Definition Language (IDL) of the WebAssembly **Component Model** — a small, language-agnostic schema language that describes the typed boundary between a component and its host (or between two components). An IDL is a language for declaring data shapes and function signatures independent of any single programming language; the same WIT file can be consumed by Rust, JavaScript, Python, Go, and others.

A WIT file looks like this:

```wit
package formawasm:demo;

interface counter {
    record state { value: s32 }
    increment: func(s: state) -> state;
    reset: func() -> state;
}

world component {
    export counter;
}
```

The Component Model is a layer above core WebAssembly. Core WebAssembly only knows `i32 / i64 / f32 / f64` and linear memory; the Component Model adds typed records, variants, lists, options, strings, and resources, and it specifies a canonical Application Binary Interface (ABI) for crossing the boundary. WIT is how you write that boundary down.

For formawasm, WIT is **auto-generated** from the public surface of each IR module — every `pub` struct, enum, and function in a `.fv` source file becomes a WIT type or function declaration. The host never hand-writes WIT.

---

## Toolchain

| Tool | Role |
|---|---|
| [`wasm-encoder`](https://docs.rs/wasm-encoder) | Emit core Wasm bytes from typed builders. |
| [`wasmparser`](https://docs.rs/wasmparser) | Validate emitted modules in tests. |
| [`wit-component`](https://docs.rs/wit-component) | Wrap a core module + WIT into a Component Model artifact. |
| [`wasmtime`](https://docs.rs/wasmtime) (dev only) | Smoke-test host for integration tests. |
| [`thiserror`](https://docs.rs/thiserror) | Backend error types. |
| [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools) | Command Line Interface (CLI) for diffing, printing, validating during development. Not a Cargo dependency — a developer aid. |

### Output format

Component Model from day one. The core WebAssembly module is an internal step, never the shipped artifact.

---

## Repo layout

formawasm is a separate repo from formalang. The two crates evolve on independent release cadences.

```text
Cargo.toml            # depends on formalang via path during dev,
                      # crates.io once IR shape stabilises
src/
    lib.rs            # WasmBackend, public entry point
    preflight.rs      # rejection of unsupported IR shapes
    survey.rs         # public-surface classification
    layout.rs         # memory-layout planning
    lower/            # IR expr → Wasm stack-machine
    wit/              # WIT auto-generation
    component.rs      # core module + WIT → component
tests/
    fixtures/         # .fv programs compiled in tests
    integration/      # wasmtime end-to-end tests
```

---

## Boundary policy

What crosses the public component boundary is a strict subset of what's supported inside a component. The boundary is defined by what WIT can express; the inside is defined by core WebAssembly's full power.

**Crosses the boundary:**

- Primitives (`I32 / I64 / F32 / F64`, `Boolean`, `String`).
- `Optional<T>`, `Array<T>`, `Dictionary<K,V>` — mapped to `option`, `list`, `list<tuple<K,V>>` respectively.
- `IrStruct` → `record`; `IrEnum` (tagged variants with payload) → `variant`.
- Named tuples → `record { name: T, ... }`. formalang tuples carry field names; we deliberately do **not** use WIT positional `tuple`.
- `Path` and `Regex` represented as WIT `string` at the boundary; identity preserved internally.

**Rejected at pre-flight (cannot cross):**

- Closure-typed values in public signatures.
- Generic traits (matches the existing `MonomorphisePass` constraint).
- Unresolved type parameters (`ResolvedType::TypeParam`) — must already be monomorphised.
- `ResolvedType::Error` — sentinel; presence indicates a frontend bug.

`pub` IR functions become component **exports**. `extern` functions (with `extern_abi`) become component **imports**. The WIT world's name is the `IrModule` name.

---

## Feature coverage

Every formalang IR construct maps to a compile phase. **Inside a module, every feature is supported**; the boundary restrictions above only apply to types in `pub` signatures.

### `IrExpr` variants

| Variant | Phase | Notes |
|---|---|---|
| `Literal` | 1a | Numeric, boolean literals; string literals in 2 |
| `Reference` (dotted path) | 1a | Resolved to local, global, or function reference |
| `LetRef` | 1a | Local-variable read |
| `SelfFieldRef` | 1b | `self.field` inside methods |
| `FieldAccess` | 1b | `obj.field` |
| `BinaryOp` (numeric / boolean / comparison) | 1a | Direct Wasm instructions |
| `BinaryOp::Add` on `String` | 2 | String concatenation runtime helper |
| `BinaryOp::Range` | 1c | Lowers to `{start, end}` pair in linear memory |
| `BinaryOp::Eq/Ne` on `String` | 2 | String equality runtime helper |
| `UnaryOp` | 1a | `Neg`, `Not` |
| `If` | 1a | Maps to Wasm `if/else` |
| `Block` | 1a | Sequence of statements + result expression |
| `For` (over `Array`) | 1c | `loop` + `br_if` with index counter |
| `For` (over `Range`) | 1c | Same lowering, no indirection through array |
| `Match` | 1b | `br_table` on enum tag, payload extraction by offset |
| `FunctionCall` (direct) | 1a | Wasm `call` instruction |
| `MethodCall` (Static dispatch) | 1b | Resolved to direct `call` at compile time |
| `MethodCall` (Virtual dispatch) | 3 | Vtable lookup + `call_indirect` |
| `StructInst` | 1b | Bump-allocate, write fields, return pointer |
| `EnumInst` | 1b | Allocate tag + payload, return pointer |
| `Array` (literal) | 1c | Allocate `{ptr, len, cap}` header + element buffer |
| `Tuple` (literal) | 1b | Treated as anonymous struct |
| `DictLiteral` | 2 | Sorted-pairs array v1 |
| `DictAccess` | 2 | Lookup runtime helper |
| `Closure` | — | Eliminated upstream by closure-conversion `IrPass` |
| `ClosureRef { funcref, env_struct }` | 1b | Synthesised by closure conversion; lowered as funcref index + env pointer |

### `ResolvedType` variants

| Type | Phase | Notes |
|---|---|---|
| `Primitive(I32/I64/F32/F64)` | 1a | Native Wasm valtypes |
| `Primitive(Boolean)` | 1a | Lowered as `i32` (0 or 1) |
| `Primitive(Never)` | 1a | Zero-sized; functions returning `Never` emit `unreachable` |
| `Primitive(String)` | 2 | Linear-memory `{ptr, len}` |
| `Primitive(Path)` | 2 | Same layout as `String`; identity preserved internally |
| `Primitive(Regex)` | 2 | Same layout as `String`; identity preserved internally |
| `Struct` | 1b | Heap-allocated record |
| `Enum` | 1b | Tag (`i32`) + padded payload |
| `Tuple` | 1b | Same layout as anonymous `Struct` |
| `Array<T>` | 1c | `{ptr, len, cap}` |
| `Range<T>` | 1c | `{start, end}` over numeric `T` |
| `Optional<T>` | 2 | Tag + payload, or null-pointer trick for reference types |
| `Dictionary<K, V>` | 2 | Sorted-pairs array v1 |
| `Closure { param_tys, return_ty }` | 1b | Funcref index + env pointer; intramodule only |
| `External { module_path, name, … }` | 4 | Component-import lowering |
| `Generic { base, args }` | — | Eliminated by upstream `MonomorphisePass` |
| `TypeParam` | — | Pre-flight rejection |
| `Trait` | — | Banned as a value at semantic time upstream |
| `Error` | — | Pre-flight rejection (frontend invariant violation) |

### `ParamConvention` variants

| Convention | Phase | Lowering |
|---|---|---|
| `Let` (default) | 1a | Pass by value (or by pointer for aggregates) |
| `Mut` | 1b | Pass pointer into caller's frame; callee mutates in place |
| `Sink` | 1b | Move semantics: caller relinquishes the buffer; callee owns it |

### `DispatchKind` variants

| Dispatch | Phase | Lowering |
|---|---|---|
| `Static { impl_id }` | 1b | Direct `call` to a known function index |
| `Virtual { trait_id, method_name }` | 3 | Per-trait vtable in linear memory; `call_indirect` |

### Pattern shapes

formalang's IR flattens patterns to variant-name + simple bindings (no nested patterns, guards, or-patterns, or range patterns at the IR level). The Wasm lowering handles this directly via `br_table` on the variant tag plus offset-based payload extraction. `BindingPattern` destructuring in `let` bindings is also flattened upstream into simple `Let` nodes.

### Operators

`BinaryOp`: `Add, Sub, Mul, Div, Mod, Lt, Gt, Le, Ge, Eq, Ne, And, Or, Range`. `UnaryOp`: `Neg, Not`. Operator lowering is **type-dispatched** — `BinaryOp::Add` on `I32` lowers to a single Wasm instruction, but on `String` it calls a concatenation runtime helper. Type-dispatch tables are part of the lowering layer.

---

## Type mapping (formalang → WIT)

Only the boundary types appear in WIT — internal types (closures, ranges) live entirely inside the core module.

| formalang | WIT |
|---|---|
| `I32` / `I64` | `s32` / `s64` |
| `U32` / `U64` | `u32` / `u64` |
| `F32` / `F64` | `f32` / `f64` |
| `Boolean` | `bool` |
| `String`, `Path`, `Regex` | `string` |
| `Optional<T>` | `option<T>` |
| `Array<T>` | `list<T>` |
| `Dictionary<K, V>` | `list<tuple<K, V>>` |
| named tuple `(x: I32, y: I32)` | `record { x: s32, y: s32 }` |
| `IrStruct` | `record` |
| `IrEnum` | `variant` |

---

## Per-module compile pipeline

For each `IrModule` passed to `WasmBackend::generate`:

1. **Pre-flight checks.** Reject leftover `IrExpr::Closure`, public closure-typed signatures, generic traits, `ResolvedType::TypeParam`, and `ResolvedType::Error`. Fail fast with a typed error.
2. **Public-surface survey.** Walk `IrModule`; classify every item as export / import / internal.
3. **Memory-layout planning.** Compute size + offsets + alignment per `ResolvedType`. Cache results.
4. **Runtime-services planning.** One linear memory; heap-pointer + frame-pointer globals; bump allocator emitted as functions in the module itself; string/dict/equality helpers as needed.
5. **Per-function lowering.** IR expression tree → Wasm stack-machine bytecode; locals allocated per `Let` plus temps. Operator lowering is type-dispatched.
6. **Section assembly.** Use `wasm-encoder` to build types / functions / memory / globals / exports / imports / data / element / table sections.
7. **Validate.** Run `wasmparser::Validator` over the emitted core module. Fail loudly if invalid.
8. **WIT generation.** Walk the public surface, emit WIT package + world.
9. **Component wrap.** Feed core module bytes + WIT to `wit-component::ComponentEncoder`.
10. **Return** the component bytes from `Backend::generate`.

---

## Roadmap

The work splits into upstream prerequisites in formalang and four phases inside formawasm.

### Upstream prerequisites in formalang

These must land in the formalang repo *before* formawasm can begin Phase 1.

#### PR 1 — Numeric specialization (~9 commits)

Replace `PrimitiveType::Number` with `I32 / I64 / F32 / F64` (uppercase to match formalang's existing PascalCase primitive convention). Integer-literal default = `I32`. Touches lexer (literal-suffix syntax + defaulting rules), Abstract Syntax Tree (AST), IR, every match site, fixtures. Standalone, lands first, benefits every future backend.

#### PR 2 — Closure-conversion `IrPass` (~10 commits)

New pass at `src/ir/closure_conv.rs`. Runs *after* `MonomorphisePass`, *before* `DeadCodeEliminationPass`. Lifts closure bodies to top-level `IrFunction`s, synthesises capture-environment `IrStruct`s, rewrites body refs to env-field access, and replaces `IrExpr::Closure` with explicit `IrExpr::ClosureRef { funcref, env_struct }`. Inputs already exist — IR lowering already collects typed captures with `ParamConvention`.

### formawasm work

#### PR 3 — Repo bootstrap (3 commits, this repo)

1. `cargo init --lib`, license files, `.gitignore`, Continuous Integration (CI) workflow mirroring formalang.
2. Add a path dependency on the local formalang checkout; re-export public IR types.
3. Stub `WasmBackend` impl `Backend::generate` as `todo!()`. One smoke test confirming the type wires up.

#### Phase 1 — v0.1 (~30 commits, target = fibonacci, struct+enum demo, sieve, basic methods)

**1a — primitives, control flow, direct calls:**
- Cargo deps: `wasm-encoder`, `wasmparser` (dev), `wit-component`, `wasmtime` (dev), `thiserror`.
- Error types + `Result` plumbing.
- Pre-flight checks with bad-IR samples (covers `Error`, `TypeParam`, leftover `Closure`, public closure types, generic traits).
- Public-surface survey.
- Type mapping for primitives + `Boolean` + `Never`.
- Module skeleton: empty validating module, one memory, heap-pointer global.
- Function-signature emission (types + function sections).
- Lower `Literal`, `Reference`, `LetRef`, `BinaryOp` (numeric/boolean/comparison), `UnaryOp`.
- Lower `Let`, `Block`.
- Lower direct `FunctionCall`.
- Lower `If`.
- WIT generation for primitive-only signatures.
- Component wrap via `wit-component::ComponentEncoder`.
- **Milestone**: fibonacci end-to-end under wasmtime.

**1b — aggregates, methods, calling conventions:**
- Memory-layout planner for `IrStruct` (offsets, alignment, total size).
- Bump-allocator runtime helper emitted into the module.
- Lower `StructInst` + `FieldAccess` + `Tuple` literal.
- Memory-layout planner for `IrEnum` (tag + padded payload).
- Lower `EnumInst`.
- Lower `Match` via `br_table` on enum tag, with payload extraction.
- Lower `SelfFieldRef`.
- Lower `MethodCall` static dispatch.
- Lowering for `ParamConvention::Mut` (pointer into caller's frame).
- Lowering for `ParamConvention::Sink` (move semantics in linear memory).
- Lower `ClosureRef` (funcref index + env pointer; intramodule only).
- WIT mapping `IrStruct` → `record`, `IrEnum` → `variant`. Roundtrip test.
- **Milestone**: a small program with structs, enums, methods, mutable parameters, and an intramodule closure runs under wasmtime.

**1c — collections and iteration:**
- Memory-layout for `Array<T>` (`{ptr, len, cap}`).
- Lower `Array` literal construction.
- Memory-layout for `Range<T>` (`{start, end}`) and `BinaryOp::Range`.
- Lower `For` over `Array` and `For` over `Range` as `loop` + `br_if`.
- WIT mapping `Array<T>` → `list<T>`. Roundtrip test.
- **Milestone**: sieve-of-Eratosthenes runs under wasmtime.

#### Phase 2 — strings, optionals, dictionaries (~14 commits)

- Memory-layout + lowering for `Optional<T>`; WIT `option<T>`.
- String memory layout `{ptr, len}`; data-section seeding for literals; equality runtime helper.
- String concatenation runtime helper (lowering for `BinaryOp::Add` on `String`).
- WIT mapping `string` + roundtrip test.
- `Path` and `Regex` mapped to `string` at boundary, identity preserved internally.
- Dictionary as sorted-pairs array v1; literal lowering; lookup runtime helper.
- Lower `DictLiteral` and `DictAccess`.
- WIT mapping `list<tuple<K, V>>` + roundtrip test.
- **Milestone**: program using strings, optionals, dictionaries, and string concatenation.

#### Phase 3 — virtual dispatch (4 commits)

- Vtable memory layout per trait (table of funcrefs).
- Element-section entries per `impl Trait for Type`.
- `DispatchKind::Virtual` lowering: load funcref from vtable + `call_indirect`.
- **Milestone**: trait-method call across two `impl`s.

#### Phase 4 — externs and external modules (~6 commits)

- WIT-import generation from each `extern_abi` `IrFunction`.
- Component import-section emission.
- Lowering for `ResolvedType::External` references (cross-module symbol resolution).
- Test harness wires host-provided imports via `wasmtime::component::Linker`.
- **Milestone**: host-provided extern called from formalang; multi-module program compiles.

#### Phase 5+ — deferred

- `wasm-opt-rs` post-pass behind a feature flag (1 commit).
- DWARF debug info — multi-commit, design first.
- Real garbage collector over the bump allocator — separate initiative.
- Async — separate initiative.

---

## Open questions (not blocking PR 1)

- Numeric-literal suffix syntax + coercion rules (decide during PR 1).
- `IrEnum` payload-variant packing: tagged-union layout details (uniform-size vs minimal-size variants).
- Stack-vs-heap split for aggregates inside the core module (which structs can live in locals).
- Default parameter values: lower as wrapper functions or expand at the call site.
- String operations beyond concat / equality: do we ship `len`, `slice`, formatting, or expect them via `extern_abi`?

---

## Status

Upstream prerequisites in formalang are **complete** (PR 1 — numeric specialization — merged as `ff2a6c1`; PR 2 — closure-conversion pass — merged as `92fdf7c`). This repo currently contains only the project spec (`README.md`) and an action-oriented plan (`PLAN.md`); PR 3 (repo bootstrap) is the next concrete work. See [PLAN.md](PLAN.md) for the immediate microcommit list.
