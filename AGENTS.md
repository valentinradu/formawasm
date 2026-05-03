# formawasm — agent guide

Brief operational guide for AI assistants working in this repo.
Strict clippy, typed errors, tests return `Result`, no panics in
production paths.

## Where things live

| File | Purpose |
|---|---|
| `README.md` | Project spec — boundary policy, type mapping, pipeline. |
| `CHANGELOG.md` | Phase-by-phase history; "Roadmap" section captures what's left. |
| `Cargo.toml` | Single source of lint levels (`[lints.*]`). |
| `clippy.toml` | Behavioral clippy thresholds and acronym list. |
| `deny.toml` | License + advisory gates (run via `make deny`). |
| `Makefile` | Local check shortcuts (`make check` runs the full suite). |
| `.github/workflows/ci.yml` | Same gates as `make`, in CI. |

## Code style

### Comments
- Short and to the point — one sentence is usually enough.
- Explain **why** a decision was made or what architectural
  constraint it satisfies.
- Never explain what the code already says (rename a variable
  instead).
- Module-level `//!` comments: purpose of the module and its
  relationship to the rest of the system. One short paragraph max.
- Struct/enum doc comments: one line stating what it represents
  and its role.
- Method/function doc comments: only when the signature does not
  make the intent obvious, or when there is a non-obvious
  invariant the caller must respect.

### Errors
- Always typed. Use `thiserror` for crate-level error enums.
- Never `unwrap()` / `expect()` / `panic!()` outside of tests
  (clippy enforces this).
- Tests return `Result<(), TestError>` where `TestError = Box<dyn
  std::error::Error + Send + Sync>`. Use `?` to propagate; never
  bare `assert!` / `panic!` — return `Err(...)` instead.

### Suppressions
- `#[allow]` is forbidden by lint. Use `#[expect(reason = "...")]`
  with a real explanation when you must override a lint.

### Async
- Only where genuinely needed. Don't make a function `async`
  speculatively.
- Never hold a lock across `.await`. Clippy enforces
  `await_holding_lock`.

## Preferred crates

- **`thiserror`** (v2) — all error handling.
- **`strum`** (with `derive`) — enum ↔ string conversions.

## Microcommit cadence

One commit per microcommit. Before each commit verify:

```bash
make check     # fmt + clippy + doc + test
```

Subject line: `<scope>: <verb-phrase>` (e.g. `lower: emit i32 add
via wasm-encoder`). Body explains *why*, not *what*.
