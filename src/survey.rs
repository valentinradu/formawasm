//! Classification of an [`IrModule`]'s top-level items into exports,
//! imports, and exported types.
//!
//! Runs after pre-flight and feeds the WIT-generation and component-
//! wrap stages. Indices into [`PublicSurface`] are stable IDs the
//! backend can hand to `wasm-encoder` directly.
//!
//! Methods declared inside `impl` blocks are not surfaced here —
//! formalang's IR addresses them through their containing
//! [`IrImpl`](formalang::ir::IrImpl), and the backend lowers them as
//! ordinary functions reachable only through method dispatch.

use formalang::ast::Visibility;
use formalang::ir::{EnumId, FunctionId, IrModule, StructId};

/// Classification of a module's top-level items at the component
/// boundary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PublicSurface {
    /// Top-level functions that become component exports. Currently
    /// every non-extern top-level function is treated as a candidate
    /// export — formalang's IR does not yet carry a `visibility` field
    /// on `IrFunction`, so we cannot distinguish private helpers from
    /// genuine exports here. WIT generation narrows further.
    pub exports: Vec<FunctionId>,

    /// Top-level extern functions (`extern_abi.is_some()`) that
    /// become component imports.
    pub imports: Vec<FunctionId>,

    /// Public structs whose layout becomes a WIT `record`.
    pub exported_structs: Vec<StructId>,

    /// Public enums whose tags + payloads become a WIT `variant`.
    pub exported_enums: Vec<EnumId>,
}

/// Classify every top-level item in `module`.
#[must_use]
pub fn survey(module: &IrModule) -> PublicSurface {
    let mut surface = PublicSurface::default();

    for (idx, f) in module.functions.iter().enumerate() {
        let id = FunctionId(u32_from_index(idx));
        if f.is_extern() {
            surface.imports.push(id);
        } else {
            surface.exports.push(id);
        }
    }

    for (idx, s) in module.structs.iter().enumerate() {
        if matches!(s.visibility, Visibility::Public) {
            surface.exported_structs.push(StructId(u32_from_index(idx)));
        }
    }

    for (idx, e) in module.enums.iter().enumerate() {
        if matches!(e.visibility, Visibility::Public) {
            surface.exported_enums.push(EnumId(u32_from_index(idx)));
        }
    }

    surface
}

/// Cast a `Vec` index to `u32` for `FunctionId` / `StructId` /
/// `EnumId` construction. The IR enforces `< u32::MAX` items in
/// `add_struct` / `add_function`; manually-constructed modules used in
/// tests are far below that ceiling, so saturating-on-overflow keeps
/// the lint config happy without a panic path.
const fn u32_from_index(idx: usize) -> u32 {
    if idx > u32::MAX as usize {
        u32::MAX
    } else {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "guarded by the bound check above"
        )]
        let cast = idx as u32;
        cast
    }
}
