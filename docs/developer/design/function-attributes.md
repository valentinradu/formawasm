# Function Attributes (`#[inline]` / `#[no_inline]` / `#[cold]`)

**Status**: design note / decision recorded
**Last updated**: 2026-05-02

formalang lets source authors annotate functions with codegen
hints — `inline`, `no_inline`, `cold` — and preserves them
through the IR as `IrFunction.attributes: Vec<FunctionAttribute>`.
This document records why the WebAssembly backend doesn't honor
those attributes today and the conditions under which the
decision should be revisited.

**Decision**: ignore function attributes; delegate all
inlining / placement decisions to `wasm-opt`. Revisit if a real
consumer surfaces a workload where the attributes' intent
materially differs from binaryen's heuristics.

---

## Why ignoring is correct today

Wasm has no first-class equivalent for any of the three
attributes:

- `Inline` / `NoInline`: no wasm instruction or section marks a
  function as "always inline" or "never inline". Inlining
  decisions live entirely in the optimizer (binaryen, wasm-opt).
- `Cold`: no wasm equivalent of a `.text.cold` section or branch-
  prediction hint. Wasm engines decide hot/cold placement
  internally based on profile data they collect at runtime.

Three options for the backend:

### Option A — pass attributes through to wasm-opt

Binaryen has no documented input format for "the source author
asked to inline this function". The closest thing — naming a
function `__attribute__((always_inline))`-style — works for C/C++
inputs but doesn't have a wasm-encoder side. We'd have to pre-
process the module ourselves, mark functions, then run a custom
pass; binaryen's stock pipeline ignores all of it.

The result would either be code that pre-empts binaryen's
inlining heuristics (rarely the right call — binaryen has good
heuristics tuned for size-vs-speed) or code that adds marker
custom sections binaryen doesn't read.

### Option B — emit a custom section listing attributes

Ship a `formawasm.attributes` custom section keyed by function
index, listing the attribute set per function. Useful for tooling
that wants to introspect the original intent (debuggers, IDE
plugins). Doesn't actually affect codegen — binaryen still ignores
the marker, the engine still ignores it.

This is plausible but speculative: nobody currently reads such a
section. It's also non-standard, so different tools would read
different formats.

### Option C — ignore and document

The IR field stays preserved through the pipeline (closure-conv,
DCE, fold). Backends that can express the attributes (LLVM IR
emitter would have `inlinehint` / `coldcc`, JVM emitter would
have nothing) honor them. The wasm backend doesn't, and says so.

This is what we're doing. Cost: zero. Benefit: zero. Risk: the
implicit user expectation that "inline" means something. Mitigated
by the source-language documentation explaining that backends
choose how to honor hints.

## What would change the call

If any of these surface, revisit:

1. **Profiling shows a real workload where binaryen's inlining
   heuristic underperforms a manual hint by a measurable margin
   (>5%).** A `--respect-inline-hints` codegen flag could then
   pre-process the module to bias binaryen's choice.
2. **A wasm proposal lands defining function attributes natively
   (e.g. branch-hint sections like `wasi-nn`'s).** Honoring would
   become a question of emitting the right wasm-spec-defined
   section.
3. **A non-codegen consumer needs the attributes.** A dataflow
   analyzer, profile-guided optimizer, or IDE plugin might want
   "user said inline" data alongside the wasm bytecode. A custom
   section with a documented format is the natural bridge.

Until then: the IR's `attributes` field is passed through for
the future, but never consulted at codegen time. No diagnostic
fires when a function is annotated; the annotation is silently
preserved in the IR shape and silently ignored by the wasm
backend.
