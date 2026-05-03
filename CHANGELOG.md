# Changelog

All user-visible changes to formawasm, grouped by phase. The
project is pre-1.0 and breaks API freely; the format here is
informational rather than semver-tracked. See `git log` for the
microcommit-level history and `PLAN.md` for the planning view.

## [Unreleased]

### Phase-5 housekeeping (2026-05-02)

- `wasm-opt` post-pass behind a `wasm-opt` cargo feature
  (`backend.rs`). Off by default — feature-on runs binaryen at
  `-Os` over emitted core wasm before component wrapping with
  `Feature::All` enabled (multi-table, reference types, bulk-
  memory).
- `wasmparser`-based optional validation via
  `WasmBackend::with_validation()`. Promoted `wasmparser` from
  dev-dep to direct dep (free — was already in graph
  transitively). New `WasmBackendError::Validation { reason }`.
- WIT multi-field variant payloads now emit as `tuple<T0, T1, …>`
  arms; previous `NotYetSupported` rejection is gone.
- For-loop over `Range<F32>` / `Range<F64>` works alongside the
  integer ranges. Step is fixed at `1.0`; output buffer sizes via
  `ceil(end - start)` so fractional gaps don't overrun.
- `tracing` (0.1) instrumentation: `#[instrument]` spans on
  `WasmBackend::generate`, `lower_module`, `build_closure_plumbing`,
  `build_vtable_plumbing` plus per-stage `tracing::debug!` events
  with byte-size data points. No-subscriber path is essentially
  free.
- Wasm `name` custom section emission: function (and import) names
  ship in the artifact so debug tooling resolves `func[N]` back to
  the source identifier.
- `formawasm-cli` binary at `src/bin/formawasm-cli.rs` — single-
  file source-to-component driver. Reads a `.fv` file, runs the
  standard codegen pipeline, writes a `.wasm` component.
- Diagnostics: `External` rejection now embeds a path to the
  upstream cross-module-codegen design note in every error
  message, so anyone hitting the rejection gets a one-hop
  explanation.
- Public-surface audit: `string_pool` demoted to `pub(crate)`; four
  unused public methods (`StringPool::header_offset`, `len`,
  `is_empty`, plus a redundant `set_string_data` rename) dropped.
- Dependency audit: bumped `wasmtime` 44.0.0 → 44.0.1 to address
  RUSTSEC-2026-0114 (panic when allocating a table exceeding the
  host address space).
- CI: parallel job under `--features wasm-opt` so the optional
  post-pass can't bit-rot.
- Layout-offset constants named:
  `ARRAY_HEADER_PTR_OFFSET` / `_LEN_OFFSET` / `_CAP_OFFSET`.
- Test coverage: 5 new variant-rejection tests
  (`tests/lower_error_coverage.rs`) plus 3 new CLI integration
  tests (`tests/cli.rs`).
- Doc refresh: module-level rustdoc on `backend.rs`, `layout.rs`,
  `types.rs`, `wit.rs` brought in line with current state. README
  / PLAN status blocks reflect Phases 1-4 closed and Phase 5
  partial.

### Upstream-blocked items (design notes pushed)

Each note lives at `~/projects/formalang/docs/developer/` on a
named branch:

- Cross-module type references (`ResolvedType::External`):
  `cross-module-codegen.md` on branch
  `cross-module-codegen-design`.
- String built-in methods (`s.len()` / `s.slice(...)`):
  `string-builtins.md` on branch `string-builtins-design`.
- Default parameter values:
  `default-parameters.md` on branch `default-params-design`.
- DWARF / source-map emission (IR carries no spans):
  `ir-spans.md` on branch `dwarf-spans-design`.

Backend lifting follows once upstream commits to one direction
per note.

### Resolved as design decisions

- **Stack-vs-heap split for small aggregates**: keep uniform heap.
  Analysis at `docs/developer/design/stack-vs-heap-aggregates.md`. Cost is
  invasive across ~25 lowering paths plus boundary trampolines;
  benefit is speculative for our workload.

## Phase 4 — externs and host-provided imports (closed 2026-05-02)

- WIT `import` line emission for every `extern_abi`-bearing
  function in the surface.
- Core-wasm import section. `ModuleBuilder::declare_function_import`
  declares a function import under `cm32p2` (canonical-ABI 32-bit-
  platform-2 mangling per `wit-component`). Imports occupy the
  leading region of the function-index space.
- Milestone: `host_double` extern called from a local `call_host`
  function under wasmtime, wired through
  `wasmtime::component::Linker::root().func_wrap(...)`.

## Phase 3 — virtual dispatch (closed 2026-05-02)

- `plan_vtable` layout planner — flat array of i32 funcref-table
  indices, one per trait method.
- Per-impl method funcref table + per-`(trait, target)` vtable
  bytes seeded into the static-data segment alongside string-pool
  data.
- `DispatchKind::Virtual` lowering — load funcref from
  `vtable_base + method_idx * 4`, push receiver + args, emit
  `call_indirect`.
- Milestone: trait `Greet` with method `value()` dispatches across
  Alpha (returns 1) and Beta (returns 2) impls under wasmtime.

## Phase 2 — strings, optionals, dictionaries (closed 2026-05-01)

- `Optional<T>` layout (uniform `{ tag, payload }` cell), `Nil`
  literal, Some-wrap coercion at let / return / if / match / args
  / aggregate fields, WIT `option<T>` mapping.
- String memory layout `{ ptr, len }`; data-section seeding for
  literals; `__str_eq` and `__str_concat` runtime helpers; WIT
  `string` mapping with canonical-ABI parameter-split wrappers
  and `cabi_realloc` export.
- `Dictionary<K, V>` v1 (sorted-pairs array of pair pointers) with
  literal construction + linear-scan lookup; WIT
  `list<tuple<K, V>>` mapping.
- Milestone: `greet(role: String) -> String` exercising a
  `Dictionary<String, String>` literal, an `I32?` Some-wrap, and
  string concatenation across the component boundary.

## Phase 1c — collections and iteration (closed 2026-04-30)

- `Array<T>` layout (`{ ptr, len, cap }` header).
- `Range<T>` literal lowering for `lo..hi`.
- `For` over `Range<T>` and `Array<T>` as `loop` + `br_if`
  comprehensions producing fresh `Array<body_ty>` results.
- WIT `list<T>` mapping with nested-list composition.
- Milestone: sieve of Eratosthenes runs under wasmtime,
  returning a `list<bool>` primality vector.

## Phase 1b — aggregates, methods, calling conventions (closed 2026-04-29)

- `IrStruct` and `IrEnum` layout planners (uniform-size variants
  with `i32` discriminant tag at offset 0).
- `StructInst` / `EnumInst` / `Tuple` / `FieldAccess` /
  `SelfFieldRef` lowering through the bump allocator.
- `Match` via `br_table` on the discriminant tag with payload-
  binding extraction into wasm locals.
- `MethodCall` static dispatch + impl walking via `MethodMap`.
- Mut-self field assignment through `IrBlockStatement::Assign`.
- `ClosureRef` as `(funcref, env_ptr)` pair in linear memory; full
  indirect closure invocation via funcref `Table` + `call_indirect`
  in post-Phase-1b housekeeping.
- WIT `record` and `variant` mapping (kebab-cased identifiers).
- Milestone: `Counter` / `Action` enum with `apply` (Match) and
  `double` (mut-self) methods runs under wasmtime.

## Phase 1a — primitives, control flow, direct calls (closed 2026-04-29)

- Pre-flight rejection of unsupported IR shapes (closures,
  generic traits, type params, error placeholder).
- Public-surface survey classifying top-level items.
- Primitive type mapping (I32 / I64 / F32 / F64 / Boolean /
  Never).
- Module skeleton: linear memory + heap-pointer global.
- Function-signature emission, `Literal` / `Reference` / `LetRef`
  / `BinaryOp` / `UnaryOp` / `Block` / `Let` / direct
  `FunctionCall` / `If` lowering.
- Module-level walker (`lower_module`).
- WIT generation for primitive-only signatures.
- Component wrap via `wit-component::ComponentEncoder`.
- Milestone: recursive fibonacci compiles, validates, and runs
  under wasmtime.

## Bootstrap (2026-04-25)

- Cargo scaffold + strict lint config (mirrors
  `~/projects/smid/smid-ws0`).
- CI workflow: fmt / clippy / test / doc / deny.
- Path dependency on local formalang checkout.
- Stub `WasmBackend` impl `Backend::generate` returning
  `todo!()`.
