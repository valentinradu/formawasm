# Contributing

Brief operational guide for working on formawasm. Strict clippy, typed errors, tests return `Result`, no panics in production paths.

## Where things live

| File | Purpose |
|---|---|
| `README.md` | Project pitch — short, user-facing. |
| `book.toml` + `docs/` | This book. Built with `mdbook build`. |
| `PLAN.md` | What to do *next*. Per-microcommit forward planning. |
| `CHANGELOG.md` | Phase-by-phase history. |
| `Cargo.toml` | Single source of lint levels (`[lints.*]`). |
| `clippy.toml` | Behavioral clippy thresholds and acronym list. |
| `deny.toml` | License + advisory gates (run via `make deny`). |
| `Makefile` | Local check shortcuts (`make check` runs the full suite). |
| `.github/workflows/ci.yml` | Same gates as `make`, in CI. |

## Code style

### Comments

- Short and to the point — one sentence is usually enough.
- Explain **why** a decision was made or what architectural constraint it satisfies.
- Never explain what the code already says (rename a variable instead).
- Module-level `//!` comments: purpose of the module and its relationship to the rest of the system. One short paragraph max.
- Struct/enum doc comments: one line stating what it represents and its role.
- Method/function doc comments: only when the signature does not make the intent obvious, or when there is a non-obvious invariant the caller must respect.

### Errors

- Always typed. Use `thiserror` for crate-level error enums.
- Never `unwrap()` / `expect()` / `panic!()` outside of tests (clippy enforces this).
- Tests return `Result<(), TestError>` where `TestError = Box<dyn std::error::Error + Send + Sync>`. Use `?` to propagate; never bare `assert!` / `panic!` — return `Err(...)` instead.

### Suppressions

- `#[allow]` is forbidden by lint. Use `#[expect(reason = "...")]` with a real explanation when you must override a lint.

### Async

- Only where genuinely needed. Don't make a function `async` speculatively.
- Never hold a lock across `.await`. Clippy enforces `await_holding_lock`.

## Preferred crates

- **`thiserror`** (v2) — all error handling.
- **`strum`** (with `derive`) — enum ↔ string conversions.

## Microcommit cadence

One commit per microcommit. Before each commit verify:

```bash
make check     # fmt + clippy + doc + test
```

Subject line: `<scope>: <verb-phrase>` (e.g. `lower: emit i32 add via wasm-encoder`). Body explains *why*, not *what*.

The full local suite — including `cargo deny` and the `wasm-opt` feature path — runs as:

```bash
make ci
```

That mirrors the GitHub Actions workflow.

## Quality gates

The strict lint configuration in `Cargo.toml::[lints.*]` is the canonical source. Highlights:

- **No silent failures**: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `exit` are all `deny`. Use typed `Result` everywhere.
- **No print macros**: `print_stdout`, `print_stderr`, `dbg_macro` are `deny`. Use `tracing` for diagnostics. The CLI binary is the one exception, gated by an `#[expect(reason = "...")]` at the binary's top.
- **No silent overflow / wrap / truncation / sign loss**: `arithmetic_side_effects`, `integer_division`, `modulo_arithmetic`, `cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss`, `cast_precision_loss` are all `deny`. Use checked arithmetic or document the invariant via `#[expect(reason = "...")]`.
- **No `await_holding_lock`** — async work that mixes locks and `.await` is a deadlock waiting to happen.
- **Match exhaustiveness**: `wildcard_enum_match_arm` is `deny`. Every enum match lists every variant explicitly.

The strict-clippy + typed-error setup mirrors the reference Rust setup at `~/projects/smid/smid-ws0`.

## CI

`.github/workflows/ci.yml` runs the same gates as `make ci`:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo doc --no-deps` with `RUSTDOCFLAGS="-D warnings"` (catches dead intra-doc links and private-item leaks)
- `cargo test`
- `cargo deny check` (license + advisory gates)
- `cargo test --features wasm-opt` (parallel job; the optional post-pass can't bit-rot)

A failing CI gate blocks merge. Don't bypass with `--no-verify` — fix the underlying issue.

## Working in the book

The book's source lives in `docs/`; build it with:

```bash
mdbook build           # output goes to ./book/
mdbook serve           # local dev server with live reload
```

`./book/` is `.gitignore`d — only the `docs/` source is committed.

When you change the book, update [`docs/SUMMARY.md`](../SUMMARY.md) if you've added or removed pages. mdBook only emits pages listed in `SUMMARY.md`, so an unlinked file silently disappears from the rendered output.
