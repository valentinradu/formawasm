# PLAN.md — formawasm

Action-oriented plan for picking up formawasm in a fresh session. The
**[README](README.md)** is the project spec (toolchain, boundary
policy, feature → phase tables, type mapping, pipeline) — read it
first, then come back here for the "what to do now" view.

---

## Status (as of 2026-04-27)

**Upstream — formalang at `~/projects/formalang`:**

- ✅ **PR 1 — Numeric specialization**: MERGED as `ff2a6c1`. `PrimitiveType` now has `I32 / I64 / F32 / F64`; integer-literal default is `I32`, float-literal default is `F64`; literal suffixes (`42I32`, `3.14F64`) supported.
- ✅ **PR 2 — Closure-conversion `IrPass`**: MERGED as `92fdf7c`. `ir::ClosureConversionPass` lifts every `IrExpr::Closure` to a top-level `IrFunction` paired with a synthesized capture-environment `IrStruct`, replacing the closure expression with `IrExpr::ClosureRef { funcref, env_struct, ty }`. Run between `MonomorphisePass` and `DeadCodeEliminationPass`.

**This repo:**

- ✅ `README.md` — full project spec.
- ❌ Everything else. **Not yet a Cargo project.** PR 3 mc1 runs `cargo init`.

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

## Immediate work — PR 3: repo bootstrap (5 commits)

Run from `~/projects/formawasm`:

### mc1 — `cargo init` + smid-ws0 quality gates

```bash
cargo init --lib --name formawasm
```

Then drop in the full smid-ws0 quality kit, adapted for a single
crate (no workspace yet):

- `Cargo.toml`: `edition = "2024"`, `rust-version = "1.93"`,
  `license = "MIT OR Apache-2.0"`, all `[lints.rust]` /
  `[lints.clippy]` / `[lints.rustdoc]` rules from
  `~/projects/smid/smid-ws0/Cargo.toml` (`unwrap_used = "deny"`,
  `expect_used = "deny"`, `panic = "deny"`, `todo = "deny"`,
  `unimplemented = "deny"`, `unreachable = "deny"`, `print_*`,
  `dbg_macro`, `arithmetic_side_effects`, lossy casts, etc.).
- `clippy.toml` — copy from smid-ws0; tweak `doc-valid-idents` to
  add `WIT`, `IR`, `WASM`, `ABI`, `IDL`, `MVP`.
- `rust-toolchain.toml` — pin `1.93` with `rustfmt`, `clippy`.
- `deny.toml` — adapt smid-ws0's; drop the smid-specific
  `RUSTSEC` ignores.
- `.cargo/config.toml` — mold linker on linux; **skip sccache**
  (don't force a tool dep on contributors).
- `.gitignore` — `/target`, `*.profraw`.
- `LICENSE-MIT` + `LICENSE-APACHE` — copy from formalang.
- `Makefile` — `check` / `fmt` / `clippy` / `doc` / `test`
  targets mirroring smid-ws0.
- `AGENTS.md` — adapted subset of smid-ws0's (preferred crates,
  comments style, env-var policy).
- `.github/workflows/ci.yml` — fmt + clippy + deny + test jobs.
  Modelled on smid-ws0; tests stay enabled (formawasm is small
  enough).
- Empty `src/lib.rs` (cargo init default is fine).

**Verify**: `cargo build` + `cargo clippy --all-targets -- -D warnings`
+ `cargo test` all green, no warnings.

### mc1.5 — create private GitHub repo + push initial commit

```bash
git add -A
git commit -m "scaffold formawasm with smid-ws0 quality gates"
gh repo create valentinradu/formawasm --private --source=. --push
```

**Verify**: `gh repo view valentinradu/formawasm` shows the repo,
CI runs and passes.

### mc2 — formalang path dep + IR re-exports

In `Cargo.toml`:

```toml
[dependencies]
formalang = { path = "../formalang" }
```

In `src/lib.rs`, re-export the types you'll need at the public
surface:

```rust
pub use formalang::ir::{IrModule, /* others as you need them */};
pub use formalang::pipeline::Backend;
```

**Verify**: `cargo build` green. (Once formalang's IR shape stabilises
on crates.io — already at `0.2` — this dep flips to a version
constraint; for now, `path` is correct.)

### mc3 — `WasmBackend` stub

```rust
// src/lib.rs (or src/backend.rs if you want it factored)
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct WasmBackend;

impl WasmBackend {
    #[must_use]
    pub const fn new() -> Self { Self }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WasmBackendError {
    #[error("not yet implemented")]
    NotYetImplemented,
}

impl Backend for WasmBackend {
    type Output = Vec<u8>;
    type Error = WasmBackendError;

    fn generate(&self, _module: &IrModule) -> Result<Self::Output, Self::Error> {
        Err(WasmBackendError::NotYetImplemented)
    }
}
```

Note: smid-ws0 lints deny `todo!()` / `unimplemented!()`, so the
stub returns a typed error instead. Add `thiserror = "2"` to
`[dependencies]` (matching smid-ws0's pin). One smoke test
confirming the type wires up, written smid-style (returns
`TestResult`, no bare `assert!`):

```rust
type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[test]
fn wasmbackend_implements_backend_trait() -> TestResult {
    let _: &dyn formalang::Backend<Output = Vec<u8>, Error = WasmBackendError> =
        &WasmBackend::new();
    Ok(())
}
```

**Verify**: `cargo build` + `cargo clippy --all-targets -- -D warnings`
+ `cargo test` green.

After mc3 lands, the repo is wired up and ready for Phase 1.

---

## After PR 3 — Phase 1 (~30 commits to v0.1)

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

## Pre-flight checks (PR 3 + Phase 1a)

These rejections live in `src/preflight.rs` (per the README's repo
layout). Implement during Phase 1a; they're the first thing
`WasmBackend::generate` calls.

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

These don't block PR 3:

- **`IrEnum` payload packing**: uniform-size variants (simpler, more
  memory) vs minimal-size with offset table (compact, more code).
  Decide before Phase 1b.
- **Stack vs heap for aggregates**: small structs can live in Wasm
  locals; large ones must be bump-allocated. Pick a size threshold
  during Phase 1b.
- **Default parameter values**: lower as wrapper functions or
  expand at the call site? Decide before any function with
  defaults reaches Phase 1a.
- **String operations beyond concat / equality**: ship `len` /
  `slice` / formatting in-module, or require them via `extern_abi`?
  Phase 2 question.

---

## Resuming this work

A fresh Claude session in `~/projects/formawasm` should:

1. Read `README.md` for the spec, then this `PLAN.md` for what's
   next.
2. Verify formalang's IR shape hasn't shifted by reading
   `~/projects/formalang/src/ir/expr.rs` and
   `~/projects/formalang/src/ir/mod.rs` for the latest variant
   shapes.
3. Run formalang's tests to confirm baseline:
   `cd ~/projects/formalang && cargo test --quiet`.
4. Start with PR 3 mc1 above.

Microcommit cadence: one commit per microcommit, descriptive message,
verify `cargo build` + `cargo clippy --all-targets -- -D warnings` +
`cargo test` green before each commit. Mirror smid-ws0's commit
style (subject in `<scope>: <verb>` form, body explaining *why*).
