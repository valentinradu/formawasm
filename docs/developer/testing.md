# Testing

formawasm's tests are the primary specification of correct behavior. The strict-clippy + typed-error setup means runtime failures are typed errors; the test suite confirms they get raised at the right shapes and that the happy path produces bytes that validate, instantiate, and run.

## Test layout

Tests live under `tests/`, one file per concern:

```text
tests/
    backend_smoke.rs       # WasmBackend::generate end-to-end
    cli.rs                 # formawasm CLI integration tests
    layout_*.rs            # one per layout family
    lower_*.rs             # one per IR variant family
    milestone_*.rs         # per-phase end-to-end milestones
    sieve.rs               # Phase 1c milestone (named for the algorithm)
    preflight.rs           # rejection cases
    survey.rs              # public-surface classification
    types.rs               # ResolvedType → wasm valtype mapping
    wit.rs                 # WIT emission
    wit_snapshots.rs       # insta snapshots of emitted WIT
    component.rs           # component wrap
    wasm_opt_size.rs       # (feature: wasm-opt) size-comparison
```

The split mirrors `src/lower/`: one source submodule, one test file. Adding a new IR variant means adding both `src/lower/<family>.rs::lower_<variant>` and `tests/lower_<variant>.rs`.

## Test conventions

Tests return `Result<(), TestError>` where `TestError = Box<dyn std::error::Error + Send + Sync>`. Use `?` to propagate errors; never bare `assert!` / `panic!` — return `Err(...)` instead.

```rust
type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[test]
fn empty_module_generates_a_valid_component() -> TestResult {
    let backend = WasmBackend::new();
    let module = IrModule::new();
    let bytes = backend.generate(&module)?;
    validate_component(&bytes)
}
```

This pattern is enforced by the `panic_in_result_fn` clippy lint. It also matches the project-wide style of typed errors over panics: a test failure is a value, not a control-flow exception.

## What every milestone test does

End-to-end milestone tests follow a consistent shape:

1. **Hand-build the IR** for the feature (see [Extending the Backend](extending.md) for the pattern).
2. **Run through the full pipeline**: typically via `Pipeline::emit(module, &WasmBackend::new())`, sometimes directly via `WasmBackend::new().generate(&module)` if no pre-codegen passes are needed.
3. **Validate as a Component-Model artifact** using `wasmparser::Validator` with `WasmFeatures::default()`.
4. **Instantiate under wasmtime** with `Config::wasm_component_model(true)`.
5. **Call exports and assert on results** using `instance.get_typed_func` (or, for richer types, the `bindgen!` macro on a generated WIT file).

Example, condensed from `tests/backend_smoke.rs`:

```rust
let bytes = backend.generate(&module)?;
validate_component(&bytes)?;

let mut config = Config::new();
config.wasm_component_model(true);
let engine = Engine::new(&config)?;
let component = Component::from_binary(&engine, &bytes)?;
let linker = Linker::<()>::new(&engine);
let mut store = Store::new(&engine, ());
let instance = linker.instantiate(&mut store, &component)?;
let fib = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "fib")?;

let (got,) = fib.call(&mut store, (10,))?;
if got != 55 {
    return Err(format!("fib(10) = {got}, want 55").into());
}
```

## Snapshot tests for WIT

`tests/wit_snapshots.rs` uses [`insta`](https://docs.rs/insta) to capture the full WIT text emitted for representative IR shapes. Each snapshot lives in `tests/snapshots/wit_snapshots__<test>.snap` and is committed alongside the test.

When the WIT emitter changes, snapshots may diverge. Run:

```bash
cargo insta review
```

to walk through diffs and accept or reject each one. CI compares against the committed snapshots and fails if any diverge without a corresponding accept.

## Running tests

The `Makefile` wraps `cargo test` with `nice -n 19 ionice -c 3` so a heavy compile doesn't starve the desktop:

```bash
make test                    # default-features
make test-wasm-opt           # with the wasm-opt cargo feature on
TEST_ARGS='--test sieve' make test    # one file
```

For CI parity:

```bash
make ci                      # fmt + clippy + doc + test + deny + test-wasm-opt
```

The `make ci` target compiles binaryen on a clean tree (the `wasm-opt` step), so expect a few minutes the first time.

## What's not tested directly

A few things deliberately don't have unit tests:

- **The bump allocator** — exercised by every milestone test that constructs an aggregate. A direct test would lock in the `__alloc` ABI, which we may want to evolve toward GC.
- **String-pool internal layout** — same. Exercised through string-literal lowering tests.
- **`__heap_ptr`'s exact starting value** — depends on static-data size; tested via "the module instantiates" rather than by spelling out the integer.

When in doubt: a new feature gets a `lower_<feature>.rs` test. A change that touches only internal layout gets covered by the existing milestone that exercises the feature end-to-end.
