//! Identifier-shape helpers shared across the WIT emitter and the
//! core-module exporter.
//!
//! Both surfaces need the same source-level → wire-level rewriting for
//! identifiers so a function exported under `my_helper` in the IR
//! lines up with `my-helper` in the emitted WIT and the wasm export
//! section. Centralising the rewrite keeps the two paths in sync.

/// Convert a name to lower-kebab-case suitable as a WIT identifier
/// and as a wasm export name that wit-component will resolve against
/// the WIT.
///
/// Splits on existing case transitions: `MyType` → `my-type`,
/// `Pair` → `pair`, `XMLParser` → `x-m-l-parser`. Underscores collapse
/// into a single hyphen so `my_helper` → `my-helper`. Module-qualified
/// names (`colors::Rgb` produced by formalang's IR-lowering for
/// `pub mod` items) flatten to a single hyphen — `colors-rgb` — so
/// the result is a valid WIT identifier.
#[must_use]
pub(crate) fn kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
        } else if ch == '_' || ch == ':' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(ch);
        }
    }
    out
}
