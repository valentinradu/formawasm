# Feature Coverage

Every formalang IR construct maps to a compile phase. The tables below record exactly what's lowered today and how. **Inside a module, every feature is supported**; the [Boundary Policy](boundary.md) restrictions apply only to types appearing in `pub` signatures.

## `IrExpr` variants

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
| `For` (over `Range`) | 1c (I32 / I64) + post-Phase-4 housekeeping (F32 / F64) | Same lowering for all four numeric primitives. Float ranges advance by `1.0` per iteration; the output buffer is sized to `ceil(end - start)` so fractional gaps don't overrun. |
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

## `ResolvedType` variants

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
| `External { module_path, name, … }` | 5+ | Upstream-blocked. `compile_to_ir_with_resolver` returns one IrModule and discards imported-module IRs; backend has nothing to resolve `External` against. |
| `Generic { base, args }` | — | Eliminated by upstream `MonomorphisePass` |
| `TypeParam` | — | Pre-flight rejection |
| `Trait` | — | Banned as a value at semantic time upstream |
| `Error` | — | Pre-flight rejection (frontend invariant violation) |

## `ParamConvention` variants

| Convention | Phase | Lowering |
|---|---|---|
| `Let` (default) | 1a | Pass by value (or by pointer for aggregates) |
| `Mut` | 1b | Pass pointer into caller's frame; callee mutates in place |
| `Sink` | 1b | Move semantics: caller relinquishes the buffer; callee owns it |

## `DispatchKind` variants

| Dispatch | Phase | Lowering |
|---|---|---|
| `Static { impl_id }` | 1b | Direct `call` to a known function index |
| `Virtual { trait_id, method_name }` | 3 | Per-trait vtable in linear memory; `call_indirect` |

## Patterns

formalang's IR flattens patterns to variant-name + simple bindings (no nested patterns, guards, or-patterns, or range patterns at the IR level). The wasm lowering handles this directly via `br_table` on the variant tag plus offset-based payload extraction. `BindingPattern` destructuring in `let` bindings is also flattened upstream into simple `Let` nodes.

## Operators

`BinaryOp`: `Add, Sub, Mul, Div, Mod, Lt, Gt, Le, Ge, Eq, Ne, And, Or, Range`. `UnaryOp`: `Neg, Not`.

Operator lowering is **type-dispatched** — `BinaryOp::Add` on `I32` lowers to a single Wasm instruction, but on `String` it calls the `__str_concat` runtime helper. The dispatch table lives in `src/lower/binary_op.rs`.

## Phase milestones

The phases referenced above correspond to the project's milestone tests. Each milestone hand-builds an `IrModule` exercising the phase's features and runs it under wasmtime end-to-end:

| Phase | Milestone test | What it proves |
|---|---|---|
| 1a | `tests/backend_smoke.rs` | Recursive fibonacci runs |
| 1b | `tests/milestone_1b.rs` | Counter struct + Action enum + methods (mut self, Match) |
| 1c | `tests/sieve.rs` | Sieve of Eratosthenes returns `list<bool>` |
| 2 | `tests/milestone_2.rs` | `greet(role: String) -> String` exercising `Dictionary<String,String>`, `I32?` Some-wrap, string concatenation |
| 3 | `tests/milestone_3.rs` | Trait `Greet` dispatches across two impls |
| 4 | `tests/milestone_4.rs` | Host-provided `host_double` extern called from `call_host` |

The full phase-by-phase history is in `CHANGELOG.md`.
