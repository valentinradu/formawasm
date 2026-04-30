//! Lowering of [`IrExpr::Literal`].

use formalang::ast::{Literal, NumberValue, PrimitiveType};
use formalang::ir::{IrExpr, ResolvedType};
use wasm_encoder::{Ieee32, Ieee64, InstructionSink, MemArg, ValType};

use super::{LowerContext, LowerError};
use crate::layout::{OPTIONAL_TAG_ALIGN, OPTIONAL_TAG_NIL, OPTIONAL_TAG_SIZE};
use crate::module::MEMORY_INDEX;

/// Lower an [`IrExpr::Literal`] onto `sink`. The resolved type carried
/// on the expression decides which `*.const` instruction is emitted.
///
/// `ctx` is required only for `Literal::Nil` — that variant allocates
/// a tag-only `Optional<Never>` value in linear memory, which needs the
/// bump-allocator function index and a fresh i32 scratch local. The
/// other literal kinds are pure stack pushes and ignore `ctx`.
pub fn lower_literal(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::Literal { value, ty } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_literal called with non-literal expression".to_owned(),
        });
    };

    // `Literal::Nil` is the one literal whose static type is non-
    // primitive (`Optional<Never>`). Handle it before the
    // primitive-only `prim` extraction below so the type-mismatch arm
    // in that match doesn't reject it.
    if matches!(value, Literal::Nil) {
        return lower_nil(ty, sink, ctx);
    }

    let prim = match ty {
        ResolvedType::Primitive(p) => *p,
        ResolvedType::Struct(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => {
            return Err(LowerError::LiteralTypeMismatch {
                kind: literal_kind_tag(value),
                ty: ty.clone(),
            });
        }
    };

    match (value, prim) {
        // Booleans always lower to i32 (0 = false, non-zero = true).
        (Literal::Boolean(b), PrimitiveType::Boolean) => {
            sink.i32_const(i32::from(*b));
        }
        (Literal::Boolean(_), other) => {
            return Err(LowerError::LiteralTypeMismatch {
                kind: "Boolean".to_owned(),
                ty: ResolvedType::Primitive(other),
            });
        }

        // Numeric literals dispatch on the declared target. Integer
        // payloads go to i32/i64 with range-checked conversion;
        // float-syntax payloads only flow into the f32/f64 arms.
        (Literal::Number(n), PrimitiveType::I32) => {
            let v = n
                .value
                .as_i32()
                .ok_or_else(|| LowerError::LiteralOutOfRange {
                    payload: number_value_string(&n.value),
                    target: PrimitiveType::I32,
                })?;
            sink.i32_const(v);
        }
        (Literal::Number(n), PrimitiveType::I64) => {
            let v = n
                .value
                .as_i64()
                .ok_or_else(|| LowerError::LiteralOutOfRange {
                    payload: number_value_string(&n.value),
                    target: PrimitiveType::I64,
                })?;
            sink.i64_const(v);
        }
        (Literal::Number(n), PrimitiveType::F32) => {
            sink.f32_const(Ieee32::from(n.value.as_f32()));
        }
        (Literal::Number(n), PrimitiveType::F64) => {
            sink.f64_const(Ieee64::from(n.value.as_f64()));
        }
        (Literal::Number(_), other) => {
            return Err(LowerError::LiteralTypeMismatch {
                kind: "Number".to_owned(),
                ty: ResolvedType::Primitive(other),
            });
        }

        // String / Path / Regex live in Phase 2 alongside heap layouts.
        (Literal::String(_), _) => {
            return Err(LowerError::NotYetImplemented {
                what: "Literal::String (Phase 2)".to_owned(),
            });
        }
        (Literal::Path(_), _) => {
            return Err(LowerError::NotYetImplemented {
                what: "Literal::Path (Phase 2)".to_owned(),
            });
        }
        (Literal::Regex { .. }, _) => {
            return Err(LowerError::NotYetImplemented {
                what: "Literal::Regex (Phase 2)".to_owned(),
            });
        }
        (Literal::Nil, _) => {
            // The early-return above intercepts the Nil case; reaching
            // this arm would mean the carried `ty` is `Primitive(_)`,
            // which the frontend never emits.
            return Err(LowerError::LiteralTypeMismatch {
                kind: "Nil".to_owned(),
                ty: ty.clone(),
            });
        }
        // Future #[non_exhaustive] Literal variants ride this arm.
        (other, _) => {
            return Err(LowerError::NotYetImplemented {
                what: format!("Literal::{}", literal_kind_tag(other)),
            });
        }
    }

    Ok(())
}

/// Lower a `Literal::Nil`.
///
/// Allocates a tag-only `Optional<Never>` value via the bump
/// allocator: 4 bytes, with the discriminant tag set to
/// [`OPTIONAL_TAG_NIL`] at offset 0. The pointer to that allocation is
/// left on the wasm operand stack as the literal's value.
///
/// `nil` carries the static type `Optional<Never>` (so the layout is
/// the same regardless of the surrounding `Optional<T>` slot it flows
/// into — consumers only ever read the tag because it is always 0).
/// We accept any `Optional<_>` shape here for flexibility, and reject
/// non-Optional `ty` as a type-mismatch since the frontend never emits
/// that form.
fn lower_nil(
    ty: &ResolvedType,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    if !matches!(ty, ResolvedType::Optional(_)) {
        return Err(LowerError::LiteralTypeMismatch {
            kind: "Nil".to_owned(),
            ty: ty.clone(),
        });
    }

    let alloc_idx = ctx.bump_allocator()?;
    let base_local = ctx.next_scratch_local(ValType::I32)?;

    sink.i32_const(i32::try_from(OPTIONAL_TAG_SIZE).unwrap_or(i32::MAX));
    sink.call(alloc_idx);
    sink.local_set(base_local);

    sink.local_get(base_local);
    sink.i32_const(i32::try_from(OPTIONAL_TAG_NIL).unwrap_or(0));
    sink.i32_store(MemArg {
        offset: 0,
        align: OPTIONAL_TAG_ALIGN.trailing_zeros(),
        memory_index: MEMORY_INDEX,
    });

    sink.local_get(base_local);
    Ok(())
}

fn literal_kind_tag(lit: &Literal) -> String {
    match lit {
        Literal::String(_) => "String".to_owned(),
        Literal::Number(_) => "Number".to_owned(),
        Literal::Boolean(_) => "Boolean".to_owned(),
        Literal::Regex { .. } => "Regex".to_owned(),
        Literal::Path(_) => "Path".to_owned(),
        Literal::Nil => "Nil".to_owned(),
        _ => "Unknown".to_owned(),
    }
}

fn number_value_string(v: &NumberValue) -> String {
    match v {
        NumberValue::Integer(n) => n.to_string(),
        NumberValue::Float(f) => f.to_string(),
        _ => "<unknown>".to_owned(),
    }
}
