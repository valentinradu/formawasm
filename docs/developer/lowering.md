# Lowering

This page is the deep-dive into how a formalang `IrExpr` becomes core wasm. The [Architecture](architecture.md) chapter covers the surrounding pipeline; this one is about the inside of `lower_module`.

## Memory model

Every aggregate value — `IrStruct`, `IrEnum`, `Tuple`, `Array` header, `Range`, `Optional`, `String` header, `Dictionary` header — lives in linear memory through a bump allocator. The wasm representation of an aggregate is a single `i32` pointer; methods, field access, match arms, and call sites pass that pointer through wasm locals.

The decision to keep all aggregates on the heap (rather than splitting small ones onto the wasm value stack) is recorded in [Stack vs Heap for Small Aggregates](design/stack-vs-heap-aggregates.md).

### Linear-memory layout

Every formawasm-emitted module has one linear memory and a `__heap_ptr` global pointing at the next free byte. The bump allocator (`__alloc(size: i32) -> i32`) returns the current `__heap_ptr` and advances it; there's no free list and no reclamation. Components that need GC bring their own — see the "Roadmap" section in `CHANGELOG.md`.

The static-data segment occupies the leading region of linear memory:

- **String-pool bytes** — every string literal interned at compile time, packed contiguously.
- **Per-impl vtables** — flat arrays of funcref-table indices, one per trait method per impl.
- **Per-literal `(ptr, len)` headers** — pre-built `String` headers pointing at the pool.

`__heap_ptr` is initialized to the byte just past the static data; bump allocations start there.

### Aggregate layouts

| Type | Layout |
|---|---|
| `IrStruct` | Fields in declaration order, each at the next offset rounded up to its alignment |
| `IrEnum` | `i32` discriminant tag at offset 0, padded payload following |
| `Tuple` | Same as `IrStruct` (anonymous record) |
| `Array<T>` | `{ ptr: i32, len: i32, cap: i32 }` header pointing at element buffer |
| `Range<T>` | `{ start: T, end: T }` |
| `Optional<T>` | `{ tag: i32, payload: T }` |
| `String` / `Path` / `Regex` | `{ ptr: i32, len: i32 }` |
| `Dictionary<K, V>` | Sorted-pairs array v1: `{ ptr, len, cap }` over `(K, V)` pairs |
| Vtable | Flat array of `i32` funcref-table indices, one per trait method |

Size and alignment follow the Component-Model canonical ABI: `bool` is 1 byte, `s32`/`f32` are 4 bytes aligned to 4, `s64`/`f64` are 8 bytes aligned to 8. Each aggregate's total size is rounded up to its own alignment.

Aggregate-typed *fields* (a struct field whose type is itself a struct) lower as 4-byte pointers — the nested aggregate gets its own bump allocation. This keeps every aggregate's layout stable regardless of how its fields are typed.

## Runtime helpers

The lowerer emits a small set of helper functions into every module. They're conceptually a runtime; technically they're just wasm functions the lowerer wires up alongside user code.

| Helper | Signature | Job |
|---|---|---|
| `__alloc` | `(size: i32) -> i32` | Bump-allocate `size` bytes, return pointer |
| `__str_eq` | `(a_ptr: i32, a_len: i32, b_ptr: i32, b_len: i32) -> i32` | Byte-equal compare; returns 0 or 1 |
| `__str_concat` | `(a_ptr: i32, a_len: i32, b_ptr: i32, b_len: i32) -> i32` | Allocate result buffer, `memory.copy` both inputs, return pointer to a fresh `{ptr, len}` header |
| `cabi_realloc` | canonical-ABI signature | Host-callable hook into `__alloc` so `wit-component`'s lift wrappers can allocate inbound buffers in our memory |

The string built-ins formalang's prelude declares (`String::len`, `is_empty`, `slice`, `starts_with`, `contains`, `byte_at`) wire to additional helpers via `prelude_helper_index`. `slice` is zero-copy (returns a fresh `{ptr, len}` header pointing into the existing buffer); `byte_at` traps on out-of-range; `contains` runs a naive O(n·m) substring search.

## Per-IrExpr lowering

`src/lower/mod.rs` declares `lower_expr`, the recursive dispatcher. Each variant routes to a function in one of the family submodules:

| Family | File | Variants |
|---|---|---|
| Aggregates | `aggregate.rs` | `StructInst`, `EnumInst`, `Tuple`, `FieldAccess` |
| Binary ops | `binary_op.rs` | `BinaryOp` (type-dispatched) |
| Blocks | `block.rs` | `Block`, `Let`, `IrBlockStatement::Assign` |
| Calls | `call.rs` | `FunctionCall`, `MethodCall` (static + virtual), `CallClosure` |
| Control | `control.rs` | `If`, `For` (over `Range` and `Array`), `Match` |
| Literals | `literal.rs` | `Literal` (numeric / boolean / string), `Array` (literal), `DictLiteral`, `nil` |
| Optional coercion | `optional.rs` | Some-wrap at let / return / if / match / args / aggregate fields |
| References | `reference.rs` | `Reference`, `LetRef`, `SelfFieldRef` |
| Unary ops | `unary_op.rs` | `Neg`, `Not` |

A few non-obvious lowering choices:

**`Match` uses `br_table` on the discriminant tag.** No string compare, no nested ifs — straight `i32_load` of the tag at offset 0, then `br_table` to the matching arm's body. Payload bindings are extracted with offset loads against the variant layout.

**`MethodCall` static dispatch becomes `call`; virtual dispatch becomes `call_indirect`.** The vtable lookup is `i32_load` from `vtable_base + method_idx * 4`, giving a funcref-table index, which `call_indirect` consumes.

**`For` over `Range<T>` and `Array<T>` both lower as `loop` + `br_if` comprehensions** producing a fresh `Array<body_ty>` result. The pre-walk (`walk_count` in `block.rs`) reserves one i32 scratch local per construction so nested allocations don't clobber each other.

**Closures, after `ClosureConversionPass`, are `(funcref, env_ptr)` pairs.** Indirect invocation via a funcref `Table` + `call_indirect` with the env pointer prepended to the user-visible argument list.

## Type-dispatched operators

Operator lowering depends on the operand type, not just the operator. `BinaryOp::Add` on `I32` lowers to `i32.add`; on `String` it calls `__str_concat`; on `F64` it lowers to `f64.add`. The dispatch table lives in `src/lower/binary_op.rs` — adding a new operand type means extending exactly one match.

Comparison operators (`Eq`, `Ne`, `Lt`, etc.) work the same way, with a separate dispatch table. `String` equality routes to `__str_eq`; numeric equality routes to the appropriate `i32.eq` / `i64.eq` / `f32.eq` / `f64.eq`.

## Boundary trampolines

Every public function gets a pair of wrappers around it:

- A **lift wrapper** that takes canonical-ABI parameters (split `(ptr, len)` for strings/lists, etc.) and assembles internal pointer arguments.
- A **lower wrapper** that takes the internal return value and produces canonical-ABI return shapes.

These are emitted by `lower_module` alongside the user function. The `cabi_realloc` export lets the host allocate inbound buffers in our linear memory before lift; this is how `wit-component` smuggles strings and lists across the boundary without copying twice.

## DWARF

Behind the `dwarf` cargo feature, `lower_module` emits four debug sections (`.debug_info`, `.debug_abbrev`, `.debug_line`, `.debug_str`). Granularity is function-level today: one subprogram DIE per user function with `name + decl_file + decl_line + low_pc / high_pc`, plus a `.debug_line` row pointing at each function's first source line. Per-statement line tables are a follow-up.

The IR-side `IrSpan` data formalang attaches to every node provides the source coordinates; `dwarf.rs` translates them into gimli's writer types and `module_lowering.rs` wires the resulting bytes into custom sections.
