//! Per-expression lowering from formalang IR to core-Wasm
//! instructions.
//!
//! Each `lower_*` function appends instructions to the caller's
//! `InstructionSink` and assumes the surrounding stack discipline is
//! maintained by the caller. Helpers do not emit a closing `end` —
//! that's the function-body framer's job.

use formalang::ast::{Literal, NumberValue, PrimitiveType};
use formalang::ir::{IrExpr, ResolvedType};
use thiserror::Error;
use wasm_encoder::{Ieee32, Ieee64, InstructionSink};

/// Errors produced by the expression-lowering helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LowerError {
    /// The expression variant or operand shape is in scope for the
    /// backend but not yet implemented in this phase.
    #[error("lowering for {what} is not yet implemented")]
    NotYetImplemented {
        /// Short tag describing the missing variant or shape.
        what: String,
    },

    /// A numeric literal whose payload is incompatible with its
    /// declared type — e.g. an `I32`-typed literal whose value is out
    /// of range or carries a float-syntax payload. Semantic analysis
    /// should catch these upstream; the variant exists so we never
    /// silently emit a wrong constant.
    #[error("numeric literal {payload} cannot be lowered as {target:?}")]
    LiteralOutOfRange {
        /// String form of the offending payload (e.g. `"3.14"` or `"2147483648"`).
        payload: String,
        /// The declared target primitive type.
        target: PrimitiveType,
    },

    /// A literal whose type doesn't match its kind, e.g. a `String`
    /// literal carrying a non-`Primitive(String)` `ty`.
    #[error("literal kind {kind} does not match declared type {ty:?}")]
    LiteralTypeMismatch {
        /// Tag for the literal kind (`"Boolean"`, `"Number(integer)"`, …).
        kind: String,
        /// The declared resolved type.
        ty: ResolvedType,
    },
}

/// Lower an [`IrExpr::Literal`] onto `sink`. The resolved type carried
/// on the expression decides which `*.const` instruction is emitted.
pub fn lower_literal(expr: &IrExpr, sink: &mut InstructionSink<'_>) -> Result<(), LowerError> {
    let IrExpr::Literal { value, ty } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_literal called with non-literal expression".to_owned(),
        });
    };

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
            return Err(LowerError::NotYetImplemented {
                what: "Literal::Nil (Phase 2)".to_owned(),
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
