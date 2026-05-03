# Stack vs Heap for Small Aggregates

**Status**: design note / decision recorded
**Last updated**: 2026-05-02

An early open question for the backend was whether aggregates
inside the core module could live in wasm locals (cheap) instead
of bump-allocated linear memory (uniform). This document records
the analysis and the decision.

**Decision**: keep the current uniform-heap design through Phase 5.
Revisit only if a real workload surfaces aggregate-allocation as a
measurable bottleneck. If it does, the work belongs in its own
phase, not as an opportunistic addition.

---

## Current shape (Phase 1b through Phase 4)

Every aggregate value — `IrStruct`, `IrEnum`, `Tuple`, `Array`
header, `Range`, `Optional`, `String` header, `Dictionary` header
— lives in linear memory through the bump allocator. The wasm
representation of an aggregate is a single `i32` pointer; methods,
field access, match arms, and call sites pass that pointer
through wasm locals.

Lowering paths that depend on this convention:

- Construction (`StructInst`, `EnumInst`, `Tuple`, `Array`, range
  binary op, `nil` literal, `Some`-wrap, `String` literal,
  `DictLiteral`): bump-allocate, write fields, push pointer.
- Field access (`FieldAccess`, `SelfFieldRef`): pointer + offset
  load.
- `Match`: scrutinee pointer in a scratch local, tag load at
  offset 0, payload binding loads at variant offsets.
- Method call: `self` pointer is the first param, treated as
  `local 0`.
- For-loops: array headers + element buffers separately allocated;
  `out_buf` and `out_header` written at finalization.
- Optional coercion (`lower_coerced`): allocate Optional cell, tag
  + payload writes, push pointer.
- Boundary lifting/lowering: canonical-ABI wrappers split string /
  list parameters into `(ptr, len)` pairs and reassemble against
  our internal header pointer; `cabi_realloc` allocates inbound
  buffers in our linear memory.

The pre-walk (`walk_count` in `block.rs`) reserves one i32 scratch
local per construction so nested allocations don't clobber each
other.

## What "stack-allocated aggregates" would mean

An alternative path: aggregates whose total size fits a budget
(say, two wasm value slots) live in wasm locals — directly as
multi-value tuples — instead of bump-allocated linear memory.
A `Point { x: I32, y: I32 }` carried as a pair of locals beats
the heap path's `alloc(8) + i32_store + i32_store + i32_load +
i32_load` round-trip.

The wins are real:

- Two locals + multi-value return is cheaper than allocate + write
  + read + read.
- The bump-allocator's monotonic pointer doesn't grow for every
  short-lived `Point`; long-running components don't bloat their
  linear memory just because a tight loop constructs lots of
  small aggregates.
- Wasm engines can keep stack-allocated locals in physical
  registers more aggressively than memory-resident values.

## Why we're not building it now

### Cost: invasive across every aggregate site

Aggregates appear in roughly twenty-five lowering paths
(everything listed in "Current shape" above, plus the symmetric
read paths). Each one needs both a heap-mode and a stack-mode
emitter, plus a discriminator at the construction site. Some
sites can't easily go stack-mode at all:

- **`mut self` methods** mutate fields through the implicit `self`
  pointer at wasm-local 0. A stack-mode `self` would need either
  multi-value parameters + multi-value returns (the callee returns
  the mutated tuple) or a pointer-back-to-caller's-locals
  convention that wasm doesn't have. Heap stays.
- **Aggregates flowing across function calls** generally need
  pointers — multi-value param/return is supported but cumbersome,
  and breaks down once the aggregate is recursive or generic.
  Pure leaf functions could opt-in but the rule for "when does
  this aggregate flow vs. stay local" is non-obvious.
- **Aggregates crossing the WIT boundary** must follow the
  canonical ABI — pointer-based, anchored in linear memory.
  `cabi_realloc` is the host's hook into our allocator. Mixing
  stack and heap at the boundary requires reassembly trampolines.
- **Aggregates inside arrays / dictionaries** are
  pointer-of-aggregate values today (the array element buffer
  holds i32 pointers). Stack-mode would change the buffer's
  element stride per type. The layout planner already handles
  this for primitives; extending to size-classed aggregates needs
  a third "stack-aggregate" bucket.

The pre-walk + scratch-local infrastructure plus the canonical-
ABI wrappers plus the optional-coercion pipeline are all wired
against pointer convention. Each picks up a parallel
discriminator. The function-body planner (`block.rs`) grows a
mode field; aggregate-construction lowerings duplicate; reads
duplicate; method dispatch grows two arms.

### Benefit: speculative for our workload

The PLAN's milestones cover programs that allocate thousands of
aggregates: the sieve allocates one `Boolean` array of length
~30; milestone_1b allocates Counter / Action enum values one per
method call; milestone_2 allocates one Dictionary + ~3 strings.
None of these benchmarks heap-aggregate allocation as a
bottleneck — the bump allocator runs in O(1), the working set
stays in cache, and wasmtime's JIT optimizes the load/store
patterns.

For a real consumer that needs the perf — say, a tight numeric
loop that constructs a `Vec2 { x: F32, y: F32 }` per iteration —
the right tooling is profiling first, not a speculative refactor.

### Composition: forces decisions in code today that shouldn't bind us

Picking a size threshold (`aggregates ≤ 8 bytes go stack`) bakes a
heuristic into every aggregate site. The threshold's right value
is workload-dependent; encoding it now forces every Phase-5+
feature to compose around the chosen threshold. A threshold that's
right for tight numeric loops is wrong for components that pass
many medium-sized aggregates around.

## What would change the call

If any of these surface, revisit:

1. **Profiling shows aggregate allocation > 5% of runtime** in a
   real consumer's workload.
2. **A wasm GC proposal lands and we adopt it.** GC'd structs
   replace the bump allocator entirely; stack-mode for small
   aggregates becomes a compile-time selector, not a parallel
   path.
3. **The wasm `memargs` proposal grows multi-result-return for
   pointer-bearing aggregates.** Cheaper to flow stack values
   across call boundaries.
4. **A user-facing `#[stack]` annotation on aggregate definitions.**
   Removes the heuristic by letting the source author opt in
   per-type.

Until then: uniform heap is correct, simple, and fast enough.

## Status

**Resolved**: we picked heap; this note records why. Phase 5+
items compose against the uniform-heap convention without
worrying about a future split. If profiling on a real workload
ever flips the call, the work spins up as its own phase with a
dedicated milestone.

