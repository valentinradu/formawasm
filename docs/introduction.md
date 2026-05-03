# Introduction

**formawasm** is a backend compiler that turns a [formalang](https://github.com/valentinradu/formalang) Intermediate Representation (IR) module into a WebAssembly **component** — a `.wasm` binary that any standards-compliant runtime can execute.

```text
formalang frontend ──► IrModule ──► formawasm ──► .wasm component ──► host runtime
```

formawasm is a *backend*: it doesn't parse `.fv` source files itself, and it doesn't run the resulting wasm. Both are jobs for other libraries (formalang for parsing, [`wasmtime`](https://wasmtime.dev) / [`wasmi`](https://github.com/wasmi-labs/wasmi) / a browser engine for execution). formawasm only emits bytes.

Every public formalang declaration becomes a typed entry point in the component's interface. The boundary is described in **WIT** ([Wasm Interface Types](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md)), the small Interface Definition Language of the Component Model. formawasm generates the WIT file automatically from the public surface of each IR module — the host never hand-writes WIT.

## Two ways to read these docs

The book is split into two halves; pick the entry point that matches what you want to do.

- **Embedding formawasm in your application** → start with [Quickstart](user/quickstart.md), then [Using the Library](user/library.md) and [Hosting a Component](user/hosting.md). The [Boundary Policy](user/boundary.md) and [Type Mapping](user/type-mapping.md) chapters explain what formalang values look like when they cross into your host code.
- **Contributing to formawasm or extending the backend** → start with [Architecture](developer/architecture.md), then [Crate Layout](developer/crate-layout.md) and [Lowering](developer/lowering.md). [Extending the Backend](developer/extending.md) covers adding new IR variants or runtime helpers; [Testing](developer/testing.md) and [Contributing](developer/contributing.md) describe the project's quality bar.

## Status

Phases 1 through 5 are closed; the backend produces a Component-Model artifact for every milestone. See [Feature Coverage](user/features.md) for the per-IR-variant breakdown and the project's `CHANGELOG.md` for a phase-by-phase history.
