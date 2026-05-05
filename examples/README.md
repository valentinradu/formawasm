# Examples

20 `.fv` programs copied from formalang's [`examples/`](https://github.com/valentinradu/formalang/tree/main/examples) directory, exercising different language features.

Each program declares one or more `pub fn` exports with primitive return types, plus a `pub fn run_checks()` that calls every other export and embeds `assert(condition: ...)` against the expected outputs. The test harness in `tests/examples.rs` instantiates each compiled component under wasmtime, wires `assert` to a host function that traps on `false`, and calls `run-checks` — so passing means every embedded assertion held.

## Status against formalang 0.0.5-beta + formawasm 0.0.1-beta

All 20 examples pass end-to-end. Run `cargo test --test examples` to verify.

| #  | File                                  |
|----|---------------------------------------|
| 01 | `01_generics_box.fv`                  |
| 02 | `02_generics_pair_result.fv`          |
| 03 | `03_closure_capture.fv`               |
| 04 | `04_higher_order.fv`                  |
| 05 | `05_mut_param.fv`                     |
| 06 | `06_sink_param.fv`                    |
| 07 | `07_recursion_deep.fv`                |
| 08 | `08_trait_dispatch.fv`                |
| 09 | `09_arrays_for_dict.fv`               |
| 10 | `10_optional_destructure_overload.fv` |
| 11 | `11_strings_path_regex.fv`            |
| 12 | `12_numeric_primitives.fv`            |
| 13 | `13_tuples.fv`                        |
| 14 | `14_array_destructure.fv`             |
| 15 | `15_modules_inline.fv`                |
| 16 | `16_extern_host.fv`                   |
| 17 | `17_generic_fn.fv`                    |
| 18 | `18_trait_compose_multi.fv`           |
| 19 | `19_closure_fields_dispatch.fv`       |
| 20 | `20_match_advanced.fv`                |

## How to run individually

```bash
cargo install formawasm
curl https://wasmtime.dev/install.sh -sSf | bash    # or: brew install wasmtime

formawasm examples/02_generics_pair_result.fv
wasmtime run --invoke 'pair-sum()' examples/02_generics_pair_result.wasm
# 3
```

`wasmtime run --invoke` only handles primitive parameter and return types and doesn't supply a host `assert` import — to exercise the embedded assertions, use the test harness:

```bash
cargo test --test examples example_02_generics_pair_result
```
