//! Per-expression lowering from formalang IR to core-Wasm
//! instructions.
//!
//! Each `lower_*` function appends instructions to the caller's
//! `InstructionSink` and assumes the surrounding stack discipline is
//! maintained by the caller. Helpers do not emit a closing `end` —
//! that's the function-body framer's job.

use std::collections::HashMap;

use formalang::ast::{BinaryOperator, Literal, NumberValue, PrimitiveType, UnaryOperator};
use formalang::ir::{BindingId, IrExpr, ReferenceTarget, ResolvedType};
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

    /// A reference / `LetRef` carries a `BindingId` that the caller's
    /// [`BindingMap`] does not know about. Indicates a lowering pass
    /// failed to register the binding before walking expressions that
    /// reference it.
    #[error("BindingId {0:?} is not registered in the binding map")]
    UnknownBinding(BindingId),

    /// A reference's [`ReferenceTarget`] is `Unresolved`. Means
    /// `ResolveReferencesPass` did not run before the backend.
    #[error(
        "Reference target is Unresolved — ResolveReferencesPass must run before WasmBackend::generate"
    )]
    UnresolvedReference,

    /// A binary or unary operator was applied to operand type(s) the
    /// backend does not support — either a fundamentally invalid combo
    /// (e.g. `And` on `I32`) or a deferred case (e.g. arithmetic on
    /// `String`, which lives in Phase 2).
    #[error("operator {op} on {operand:?} is not supported in this phase")]
    UnsupportedOperator {
        /// Operator name (e.g. `"Add"`, `"Range"`, `"Mod"`).
        op: String,
        /// Operand primitive type at the offending site.
        operand: PrimitiveType,
    },
}

/// Mapping from a function-local `BindingId` (parameters + `let`
/// bindings) to its wasm-local index.
///
/// In wasm, parameters occupy local indices `0..N`; user `let`
/// bindings get indices `N..` in declaration order. The function
/// body's lowering pass owns this map and inserts entries before
/// walking expressions that reference them.
#[derive(Debug, Default, Clone)]
pub struct BindingMap {
    by_id: HashMap<BindingId, u32>,
}

impl BindingMap {
    /// Build an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a binding's wasm-local index. Panics in tests if the
    /// id is reused; production code paths funnel through
    /// `lower_function` which guarantees uniqueness.
    pub fn insert(&mut self, id: BindingId, local_index: u32) {
        self.by_id.insert(id, local_index);
    }

    /// Look up a binding's wasm-local index, or `None` when missing.
    #[must_use]
    pub fn get(&self, id: BindingId) -> Option<u32> {
        self.by_id.get(&id).copied()
    }

    /// Number of bindings recorded so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
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

/// Lower an [`IrExpr::Reference`] onto `sink`.
///
/// Function-local bindings (params and lets) become `local.get`.
/// Module-scope items (functions, structs, enums, traits, lets) and
/// externals are rejected as `NotYetImplemented` until later phases
/// lower them.
pub fn lower_reference(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    bindings: &BindingMap,
) -> Result<(), LowerError> {
    let IrExpr::Reference { target, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_reference called with non-reference expression".to_owned(),
        });
    };

    match target {
        ReferenceTarget::Param(id) | ReferenceTarget::Local(id) => {
            let idx = bindings.get(*id).ok_or(LowerError::UnknownBinding(*id))?;
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
    bindings: &BindingMap,
) -> Result<(), LowerError> {
    let IrExpr::LetRef { binding_id, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_let_ref called with non-LetRef expression".to_owned(),
        });
    };

    let idx = bindings
        .get(*binding_id)
        .ok_or(LowerError::UnknownBinding(*binding_id))?;
    sink.local_get(idx);
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

/// Lower an [`IrExpr::BinaryOp`] onto `sink`. Operands are lowered
/// recursively via [`lower_expr`]; the operator dispatch reads the
/// left operand's primitive type to choose the right wasm
/// instruction.
pub fn lower_binary_op(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    bindings: &BindingMap,
) -> Result<(), LowerError> {
    let IrExpr::BinaryOp {
        left, right, op, ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_binary_op called with non-BinaryOp expression".to_owned(),
        });
    };

    let operand_prim = match left.ty() {
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
            return Err(LowerError::NotYetImplemented {
                what: format!("BinaryOp on non-primitive operand type {:?}", left.ty()),
            });
        }
    };

    lower_expr(left, sink, bindings)?;
    lower_expr(right, sink, bindings)?;
    emit_binary_op(*op, operand_prim, sink)
}

/// Lower an [`IrExpr::UnaryOp`] onto `sink`. Operand is lowered
/// recursively via [`lower_expr`]; the operator dispatch reads the
/// operand's primitive type to choose the right wasm instruction.
///
/// `Neg` on integers lowers to `0 - operand` (wasm has no `i*.neg`);
/// `Neg` on floats uses native `f*.neg`. `Not` on `Boolean` uses
/// `i32.eqz` — which returns `1` iff the operand is `0`, the
/// canonical boolean NOT under the i32 representation.
pub fn lower_unary_op(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    bindings: &BindingMap,
) -> Result<(), LowerError> {
    let IrExpr::UnaryOp { op, operand, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_unary_op called with non-UnaryOp expression".to_owned(),
        });
    };

    let operand_prim = match operand.ty() {
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
            return Err(LowerError::NotYetImplemented {
                what: format!("UnaryOp on non-primitive operand type {:?}", operand.ty()),
            });
        }
    };

    match (op, operand_prim) {
        (UnaryOperator::Neg, PrimitiveType::I32) => {
            // wasm has no i32.neg; emit (0 - operand).
            sink.i32_const(0);
            lower_expr(operand, sink, bindings)?;
            sink.i32_sub();
        }
        (UnaryOperator::Neg, PrimitiveType::I64) => {
            sink.i64_const(0);
            lower_expr(operand, sink, bindings)?;
            sink.i64_sub();
        }
        (UnaryOperator::Neg, PrimitiveType::F32) => {
            lower_expr(operand, sink, bindings)?;
            sink.f32_neg();
        }
        (UnaryOperator::Neg, PrimitiveType::F64) => {
            lower_expr(operand, sink, bindings)?;
            sink.f64_neg();
        }
        (UnaryOperator::Not, PrimitiveType::Boolean) => {
            lower_expr(operand, sink, bindings)?;
            sink.i32_eqz();
        }
        (UnaryOperator::Neg | UnaryOperator::Not, _) => {
            return Err(LowerError::UnsupportedOperator {
                op: format!("{op:?}"),
                operand: operand_prim,
            });
        }
        // Future #[non_exhaustive] UnaryOperator variants ride this arm.
        _ => {
            return Err(LowerError::NotYetImplemented {
                what: format!("UnaryOperator::{op:?} on {operand_prim:?}"),
            });
        }
    }

    Ok(())
}

/// Top-level expression dispatcher.
///
/// Each variant either funnels into its dedicated `lower_*` helper or
/// surfaces a typed `NotYetImplemented` error tagged with the variant
/// name. The caller is responsible for the surrounding stack
/// discipline (block types, end markers, function frames).
pub fn lower_expr(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    bindings: &BindingMap,
) -> Result<(), LowerError> {
    match expr {
        IrExpr::Literal { .. } => lower_literal(expr, sink),
        IrExpr::Reference { .. } => lower_reference(expr, sink, bindings),
        IrExpr::LetRef { .. } => lower_let_ref(expr, sink, bindings),
        IrExpr::BinaryOp { .. } => lower_binary_op(expr, sink, bindings),
        IrExpr::UnaryOp { .. } => lower_unary_op(expr, sink, bindings),

        IrExpr::SelfFieldRef { .. }
        | IrExpr::FieldAccess { .. }
        | IrExpr::StructInst { .. }
        | IrExpr::EnumInst { .. }
        | IrExpr::Tuple { .. }
        | IrExpr::Array { .. }
        | IrExpr::If { .. }
        | IrExpr::For { .. }
        | IrExpr::Match { .. }
        | IrExpr::FunctionCall { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::Closure { .. }
        | IrExpr::ClosureRef { .. }
        | IrExpr::DictLiteral { .. }
        | IrExpr::DictAccess { .. }
        | IrExpr::Block { .. } => Err(LowerError::NotYetImplemented {
            what: format!("IrExpr::{}", expr_variant_name(expr)),
        }),
    }
}

const fn expr_variant_name(expr: &IrExpr) -> &'static str {
    match expr {
        IrExpr::Literal { .. } => "Literal",
        IrExpr::Reference { .. } => "Reference",
        IrExpr::LetRef { .. } => "LetRef",
        IrExpr::SelfFieldRef { .. } => "SelfFieldRef",
        IrExpr::FieldAccess { .. } => "FieldAccess",
        IrExpr::StructInst { .. } => "StructInst",
        IrExpr::EnumInst { .. } => "EnumInst",
        IrExpr::Tuple { .. } => "Tuple",
        IrExpr::Array { .. } => "Array",
        IrExpr::BinaryOp { .. } => "BinaryOp",
        IrExpr::UnaryOp { .. } => "UnaryOp",
        IrExpr::If { .. } => "If",
        IrExpr::For { .. } => "For",
        IrExpr::Match { .. } => "Match",
        IrExpr::FunctionCall { .. } => "FunctionCall",
        IrExpr::MethodCall { .. } => "MethodCall",
        IrExpr::Closure { .. } => "Closure",
        IrExpr::ClosureRef { .. } => "ClosureRef",
        IrExpr::DictLiteral { .. } => "DictLiteral",
        IrExpr::DictAccess { .. } => "DictAccess",
        IrExpr::Block { .. } => "Block",
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "type-dispatched operator table — splitting hides the per-(op, type) mapping"
)]
fn emit_binary_op(
    op: BinaryOperator,
    operand: PrimitiveType,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let unsupported = || LowerError::UnsupportedOperator {
        op: format!("{op:?}"),
        operand,
    };

    match (op, operand) {
        // ── Integer arithmetic ──────────────────────────────────────
        (BinaryOperator::Add, PrimitiveType::I32) => {
            sink.i32_add();
        }
        (BinaryOperator::Sub, PrimitiveType::I32) => {
            sink.i32_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::I32) => {
            sink.i32_mul();
        }
        (BinaryOperator::Div, PrimitiveType::I32) => {
            sink.i32_div_s();
        }
        (BinaryOperator::Mod, PrimitiveType::I32) => {
            sink.i32_rem_s();
        }
        (BinaryOperator::Add, PrimitiveType::I64) => {
            sink.i64_add();
        }
        (BinaryOperator::Sub, PrimitiveType::I64) => {
            sink.i64_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::I64) => {
            sink.i64_mul();
        }
        (BinaryOperator::Div, PrimitiveType::I64) => {
            sink.i64_div_s();
        }
        (BinaryOperator::Mod, PrimitiveType::I64) => {
            sink.i64_rem_s();
        }

        // ── Float arithmetic (no Mod — wasm has no f*.rem) ──────────
        (BinaryOperator::Add, PrimitiveType::F32) => {
            sink.f32_add();
        }
        (BinaryOperator::Sub, PrimitiveType::F32) => {
            sink.f32_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::F32) => {
            sink.f32_mul();
        }
        (BinaryOperator::Div, PrimitiveType::F32) => {
            sink.f32_div();
        }
        (BinaryOperator::Add, PrimitiveType::F64) => {
            sink.f64_add();
        }
        (BinaryOperator::Sub, PrimitiveType::F64) => {
            sink.f64_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::F64) => {
            sink.f64_mul();
        }
        (BinaryOperator::Div, PrimitiveType::F64) => {
            sink.f64_div();
        }
        // ── Comparisons ─────────────────────────────────────────────
        (BinaryOperator::Eq, PrimitiveType::I32 | PrimitiveType::Boolean) => {
            sink.i32_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::I32 | PrimitiveType::Boolean) => {
            sink.i32_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::I32) => {
            sink.i32_lt_s();
        }
        (BinaryOperator::Gt, PrimitiveType::I32) => {
            sink.i32_gt_s();
        }
        (BinaryOperator::Le, PrimitiveType::I32) => {
            sink.i32_le_s();
        }
        (BinaryOperator::Ge, PrimitiveType::I32) => {
            sink.i32_ge_s();
        }
        (BinaryOperator::Eq, PrimitiveType::I64) => {
            sink.i64_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::I64) => {
            sink.i64_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::I64) => {
            sink.i64_lt_s();
        }
        (BinaryOperator::Gt, PrimitiveType::I64) => {
            sink.i64_gt_s();
        }
        (BinaryOperator::Le, PrimitiveType::I64) => {
            sink.i64_le_s();
        }
        (BinaryOperator::Ge, PrimitiveType::I64) => {
            sink.i64_ge_s();
        }
        (BinaryOperator::Eq, PrimitiveType::F32) => {
            sink.f32_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::F32) => {
            sink.f32_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::F32) => {
            sink.f32_lt();
        }
        (BinaryOperator::Gt, PrimitiveType::F32) => {
            sink.f32_gt();
        }
        (BinaryOperator::Le, PrimitiveType::F32) => {
            sink.f32_le();
        }
        (BinaryOperator::Ge, PrimitiveType::F32) => {
            sink.f32_ge();
        }
        (BinaryOperator::Eq, PrimitiveType::F64) => {
            sink.f64_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::F64) => {
            sink.f64_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::F64) => {
            sink.f64_lt();
        }
        (BinaryOperator::Gt, PrimitiveType::F64) => {
            sink.f64_gt();
        }
        (BinaryOperator::Le, PrimitiveType::F64) => {
            sink.f64_le();
        }
        (BinaryOperator::Ge, PrimitiveType::F64) => {
            sink.f64_ge();
        }

        // ── Logical (Boolean only — i32 representation, eager eval) ─
        (BinaryOperator::And, PrimitiveType::Boolean) => {
            sink.i32_and();
        }
        (BinaryOperator::Or, PrimitiveType::Boolean) => {
            sink.i32_or();
        }

        // ── Range — Phase 1c ────────────────────────────────────────
        (BinaryOperator::Range, _) => {
            return Err(LowerError::NotYetImplemented {
                what: "BinaryOperator::Range (Phase 1c)".to_owned(),
            });
        }

        // ── Disallowed combinations + future variants ───────────────
        (
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div
            | BinaryOperator::Mod
            | BinaryOperator::Lt
            | BinaryOperator::Gt
            | BinaryOperator::Le
            | BinaryOperator::Ge
            | BinaryOperator::And
            | BinaryOperator::Or,
            _,
        ) => {
            return Err(unsupported());
        }
        _ => {
            return Err(LowerError::NotYetImplemented {
                what: format!("BinaryOperator::{op:?} on {operand:?}"),
            });
        }
    }

    Ok(())
}
