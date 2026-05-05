# Examples

20 `.fv` programs copied from formalang's [`examples/`](https://github.com/valentinradu/formalang/tree/main/examples) directory, exercising different language features.

Each program declares one or more `pub fn` exports with primitive return types, plus a `pub fn run_checks()` that calls every other export and embeds `assert(condition: ...)` against the expected outputs. The test harness in `tests/examples.rs` instantiates each compiled component under wasmtime, wires `assert` to a host function that traps on `false`, and calls `run-checks` — so passing means every embedded assertion held.

## Status against formalang 0.0.4-beta + formawasm 0.0.1-beta

Run `cargo test --test examples` to see live status. Currently:

| # | File | Result | Gap if failing |
|---|---|---|---|
| 01 | `01_generics_box.fv` | ❌ | `self` parameter typing in inherent impls (frontend) |
| 02 | `02_generics_pair_result.fv` | ✅ |  |
| 03 | `03_closure_capture.fv` | ❌ | Closure as boundary type (backend) |
| 04 | `04_higher_order.fv` | ❌ | Closure as boundary type (backend) |
| 05 | `05_mut_param.fv` | ❌ | `self` parameter typing in inherent impls (frontend) |
| 06 | `06_sink_param.fv` | ❌ | Closure boundary; struct boundary (backend) |
| 07 | `07_recursion_deep.fv` | ✅ |  |
| 08 | `08_trait_dispatch.fv` | ❌ | Trait as value type at boundary (backend) |
| 09 | `09_arrays_for_dict.fv` | ❌ | `Optional<T>` `TypeParam(T)` survives MonomorphisePass (frontend) |
| 10 | `10_optional_destructure_overload.fv` | ❌ | Same as 09 |
| 11 | `11_strings_path_regex.fv` | ❌ | `Literal::Path` lowering (backend) |
| 12 | `12_numeric_primitives.fv` | ❌ | Same as 09 |
| 13 | `13_tuples.fv` | ❌ | Tuple field access reaches the backend with `Tuple([...])` left untyped (frontend) |
| 14 | `14_array_destructure.fv` | ❌ | Empty record from tuple-arg destructuring trips wit-component validator |
| 15 | `15_modules_inline.fv` | ❌ | Struct as boundary type (backend) |
| 16 | `16_extern_host.fv` | ❌ | Extern-method registration in the static-dispatch table (backend) |
| 17 | `17_generic_fn.fv` | ❌ | Same as 09 |
| 18 | `18_trait_compose_multi.fv` | ❌ | Trait as value type at boundary (backend) |
| 19 | `19_closure_fields_dispatch.fv` | ❌ | Closure-typed `pub struct` field fails preflight (by design — closures don't cross the WIT boundary) |
| 20 | `20_match_advanced.fv` | ❌ | `Enum` as WIT-boundary type (backend) |

Two pass cleanly today (recursion + primitive containers + monomorphised generics over primitives). The remaining 18 split between **frontend gaps** (formalang regressions or features pending), **backend gaps** (lowering paths not yet wired), and **boundary-policy rejections** (#19 is correct behavior — closures can't cross WIT).

## How to run individually

```bash
cargo install --git https://github.com/valentinradu/formawasm formawasm
curl https://wasmtime.dev/install.sh -sSf | bash    # or: brew install wasmtime

formawasm examples/02_generics_pair_result.fv
wasmtime run --invoke 'pair-sum()' examples/02_generics_pair_result.wasm
# 3
```

`wasmtime run --invoke` only handles primitive parameter and return types and doesn't supply a host `assert` import — to exercise the embedded assertions, use the test harness:

```bash
cargo test --test examples example_02_generics_pair_result
```
