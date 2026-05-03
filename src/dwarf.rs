//! DWARF debug-info emission for the wasm core module.
//!
//! Gated on the `dwarf` cargo feature.
//!
//! The IR upstream (since `IrSpan { span, file: FileId }` landed)
//! carries source spans on every node and a `file_table` on
//! `IrModule`; this module turns those into `.debug_info` /
//! `.debug_abbrev` / `.debug_line` / `.debug_str` custom sections
//! attached to the emitted module.
//!
//! Granularity is function-level: one subprogram DIE per user
//! function (name, `decl_file`, `decl_line`, `low_pc`, `high_pc`)
//! plus a single `.debug_line` row per function pointing at its
//! first source line. Per-statement line tables can layer on later
//! by recording a `(span, code-section-byte-offset)` pair per
//! emitted instruction during lowering and feeding those into the
//! line program here.
//!
//! DWARF code addresses are byte offsets from the start of the wasm
//! `code` section's payload (i.e. excluding the section-id byte and
//! the section-size LEB). Every `low_pc` / `high_pc` / `.debug_line`
//! address in the output uses that frame.

use std::path::Path;

use formalang::ir::FileId;

/// Minimum information needed to emit one subprogram DIE plus its
/// line-table row.
///
/// Collected during [`crate::module_lowering::lower_module`] as each
/// user function is added to the [`crate::module::ModuleBuilder`]
/// and handed off to [`emit_debug_sections`] once every function's
/// byte range is known.
#[expect(
    clippy::exhaustive_structs,
    reason = "stable shape — every field is required to emit a subprogram DIE"
)]
#[derive(Clone, Debug)]
pub struct FunctionDebugInfo {
    /// User-visible function name (`snake_case`, pre-kebab-case).
    pub name: String,
    /// Originating source file. [`FileId::SYNTHETIC`] is skipped at
    /// emission time — closure-converted lift wrappers and the
    /// like get no DIE.
    pub file: FileId,
    /// One-indexed source line of the function definition, taken
    /// from `IrFunction.span.span.start.line`.
    pub line: u32,
    /// Function start offset within the wasm code-section payload,
    /// in bytes.
    pub code_offset_start: u32,
    /// Function end offset (exclusive) within the wasm code-section
    /// payload, in bytes.
    pub code_offset_end: u32,
}

/// Encoded `.debug_*` custom sections, ready to attach to the wasm
/// module.
///
/// Each `Vec<u8>` is the raw section payload (no section-id byte,
/// no length prefix — `wasm_encoder::CustomSection` adds those).
#[expect(
    clippy::exhaustive_structs,
    reason = "stable shape — every field corresponds to one DWARF section"
)]
#[derive(Clone, Debug, Default)]
pub struct DebugSections {
    /// `.debug_info` payload — compile units + subprogram DIEs.
    pub debug_info: Vec<u8>,
    /// `.debug_abbrev` payload — abbreviation declarations referenced
    /// from `.debug_info`.
    pub debug_abbrev: Vec<u8>,
    /// `.debug_line` payload — function-entry-granularity line table.
    pub debug_line: Vec<u8>,
    /// `.debug_str` payload — pooled strings (file paths + function
    /// names) referenced from `.debug_info` and `.debug_line`.
    pub debug_str: Vec<u8>,
}

/// Build the four `.debug_*` sections from the IR-collected debug
/// info plus the module's file table.
///
/// Returns empty sections when `functions` is empty — callers can
/// still attach the (empty) sections, but typically should skip
/// them in that case.
#[must_use]
pub fn emit_debug_sections(
    functions: &[FunctionDebugInfo],
    file_table: &[std::path::PathBuf],
) -> DebugSections {
    // Emission lives in a separate function so the public surface
    // stays a thin wrapper. The body lands in the next U7 commit
    // (gimli-backed encoder) — for now the scaffolding returns
    // empty sections so the rest of the pipeline can wire up.
    let _ = (functions, file_table);
    DebugSections::default()
}

/// Resolve a [`FileId`] against the module's file table.
///
/// Returns `None` for [`FileId::SYNTHETIC`] or any out-of-range id
/// (defensive — upstream invariants should prevent the latter).
#[must_use]
pub fn resolve_file(file: FileId, file_table: &[std::path::PathBuf]) -> Option<&Path> {
    if file.is_synthetic() {
        return None;
    }
    let idx = usize::try_from(file.0.checked_sub(1)?).ok()?;
    file_table.get(idx).map(std::path::PathBuf::as_path)
}
