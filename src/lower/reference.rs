//! Lowering of [`IrExpr::Reference`] and [`IrExpr::LetRef`].

use formalang::ir::{IrExpr, ReferenceTarget};
use wasm_encoder::InstructionSink;

use super::{LowerContext, LowerError};

/// Lower an [`IrExpr::Reference`] onto `sink`.
///
/// Function-local bindings (params and lets) become `local.get`.
/// Module-scope items (functions, structs, enums, traits, lets) and
/// externals are rejected as `NotYetImplemented` until later phases
/// lower them.
pub fn lower_reference(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::Reference { target, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_reference called with non-reference expression".to_owned(),
        });
    };

    match target {
        ReferenceTarget::Param(id) | ReferenceTarget::Local(id) => {
            let idx = ctx
                .bindings
                .get(*id)
                .ok_or(LowerError::UnknownBinding(*id))?;
            sink.local_get(idx);
            Ok(())
        }
        ReferenceTarget::Function(_) => Err(LowerError::NotYetImplemented {
            what: "Reference -> Function (closure-conv produces ClosureRef instead)".to_owned(),
        }),
        ReferenceTarget::ModuleLet(_) => Err(LowerError::NotYetImplemented {
            what: "Reference -> ModuleLet (Phase 1a)".to_owned(),
        }),
        ReferenceTarget::Struct(_) | ReferenceTarget::Enum(_) | ReferenceTarget::Trait(_) => {
            Err(LowerError::NotYetImplemented {
                what: "Reference -> type definition is not a runtime value".to_owned(),
            })
        }
        ReferenceTarget::External { .. } => Err(LowerError::NotYetImplemented {
            what: "Reference -> External (Phase 4)".to_owned(),
        }),
        ReferenceTarget::Unresolved => Err(LowerError::UnresolvedReference),
    }
}

/// Lower an [`IrExpr::LetRef`] onto `sink`. Always emits `local.get`
/// for the referenced binding's wasm-local index.
pub fn lower_let_ref(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::LetRef { binding_id, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_let_ref called with non-LetRef expression".to_owned(),
        });
    };

    let idx = ctx
        .bindings
        .get(*binding_id)
        .ok_or(LowerError::UnknownBinding(*binding_id))?;
    sink.local_get(idx);
    Ok(())
}
