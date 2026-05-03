# Troubleshooting

Common errors when compiling formalang to a WebAssembly component, and what they mean.

## Pre-flight rejections

Pre-flight runs first; failures here mean the IR shape violates an invariant the backend expects. Each one points at the IR variant that triggered it.

### `PreflightError::ClosureExprPresent`

A bare `IrExpr::Closure` survived into the backend.

**Cause**: the codegen pipeline didn't run `ClosureConversionPass` before invoking the backend. The pass lifts every closure to a top-level function plus a synthetic env struct, leaving only `IrExpr::ClosureRef` for the backend to consume.

**Fix**: ensure your `Pipeline` includes `ClosureConversionPass` between `MonomorphisePass` and `DeadCodeEliminationPass`. The `formawasm-cli` binary wires the canonical sequence; if you're driving the backend directly, mirror it.

### `PreflightError::PublicClosureSignature`

A `pub fn` has a closure-typed parameter or return value.

**Cause**: closures can't cross the WIT boundary — there's no canonical-ABI representation for "function pointer + environment". See [Boundary Policy](boundary.md).

**Fix**: make the function private (drop `pub`), or replace the closure parameter with a concrete enum / struct that the host can construct.

### `PreflightError::TypeParamPresent` / `PreflightError::GenericTraitPresent`

An unresolved type parameter or a generic trait survived into the backend.

**Cause**: `MonomorphisePass` is responsible for specializing every generic before codegen. If one slips through, the pass either didn't run or hit a bug.

**Fix**: confirm `MonomorphisePass` runs first in your pipeline. If it does and the error persists, it's an upstream bug — open an issue with the source that reproduces it.

### `PreflightError::ErrorTypePresent`

A `ResolvedType::Error` sentinel reached the backend.

**Cause**: this is an internal placeholder the formalang frontend uses while error recovery is in flight. Its presence in the IR returned to a backend means the frontend produced a partially-typed module despite `compile_to_ir` returning `Ok`.

**Fix**: open an issue against [formalang](https://github.com/valentinradu/formalang) — a successful compile should never carry `Error` types into the IR.

## Lowering errors

These surface from `lower_module` and indicate a feature the backend doesn't yet support, or a malformed IR shape.

### `WitEmitError::NotYetSupported`

The WIT emitter encountered a public-surface type it can't represent today. Most boundary-relevant types are supported (see [Type Mapping](type-mapping.md)); this error generally means a feature still in flight.

**Fix**: check the [Feature Coverage](features.md) tables. If the variant isn't listed as supported, the workaround is to keep that type internal — drop the `pub` qualifier on the offending declaration.

### `LayoutError::*`

The layout planner couldn't compute a memory layout for a type. Usually because the IR points at a struct/enum that wasn't registered in the module — typically a bug in the IR construction.

**Fix**: if you hand-built the IR, confirm every `StructId` / `EnumId` referenced from a `ResolvedType` exists in `module.structs` / `module.enums`. If you used `compile_to_ir`, this is an upstream bug.

## Component-wrap errors

### `ComponentWrapError`

`wit-component` failed to wrap the core module + WIT into a Component-Model artifact. Usually the WIT and the core module disagree about a function signature — the canonical-ABI lowering didn't match the WIT type.

**Fix**: this is a backend bug. Run with the validation step on (`WasmBackend::new().with_validation()`) to surface the malformed wasm earlier. Open an issue with the source that reproduces.

## Runtime errors (under wasmtime)

Errors during component instantiation or function calls aren't formawasm errors per se — they come from wasmtime — but a few are common enough to call out.

### "import `cm32p2::host-foo` not provided"

Your component declares an `extern fn host_foo` but the host didn't supply it via `Linker::root().func_wrap(...)`.

**Fix**: see the [Hosting a Component](hosting.md) chapter on supplying imports.

### "function not found: `foo`"

You called `instance.get_typed_func(&mut store, "foo")` but the WIT export name is `foo-bar` (kebab-case).

**Fix**: WIT identifiers are kebab-case; formalang `pub fn foo_bar` exports as `foo-bar`. See [Type Mapping](type-mapping.md).

### unexpected trap

A wasm trap — typically an arithmetic check (`i32_div_s` by zero, integer overflow on signed division, out-of-bounds index access) or an `unreachable` instruction (functions with `Never` return type). The trap's `BacktraceFrame` chain should point at the offending function.

**Fix**: enable the `dwarf` feature for source-level line numbers in the trap backtrace.

## Where to ask

- Backend bugs / feature requests: [formawasm issues](https://github.com/valentinradu/formawasm/issues)
- Frontend / language questions: [formalang issues](https://github.com/valentinradu/formalang/issues)
- Component Model / WIT questions: [bytecodealliance/wit-component](https://github.com/bytecodealliance/wasm-tools/tree/main/crates/wit-component)
