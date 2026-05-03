//! Pre-flight rejection of IR shapes the backend cannot handle.
//!
//! Runs as the first step inside `WasmBackend::generate`. The goal is
//! to fail fast with a typed error when an upstream pass was skipped
//! (monomorphisation, closure conversion) or when the module crosses
//! the component boundary with a type that cannot be expressed in
//! WIT.
//!
//! Each rejection returns a [`PreflightError`] variant carrying a
//! human-readable breadcrumb so the surfaced diagnostic points at the
//! offending item.

use formalang::ast::Visibility;
use formalang::ir::{
    IrBlockStatement, IrExpr, IrField, IrFunction, IrImpl, IrLet, IrMatchArm, IrModule, IrStruct,
    IrTrait, ResolvedType,
};
use thiserror::Error;

/// Errors produced by [`check`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PreflightError {
    /// An [`IrExpr::Closure`] survived `ClosureConversionPass`. The
    /// backend only accepts the post-conversion [`IrExpr::ClosureRef`]
    /// form.
    #[error(
        "unconverted IrExpr::Closure in {location}; ClosureConversionPass must run before WasmBackend::generate"
    )]
    UnconvertedClosure {
        /// Breadcrumb pointing at the offending expression.
        location: String,
    },

    /// A public struct field has a closure type. Closures cannot cross
    /// the WIT boundary.
    #[error(
        "pub struct '{struct_name}' field '{field}' has closure type — closures cannot cross the WIT boundary"
    )]
    PublicClosureField {
        /// Name of the offending struct.
        struct_name: String,
        /// Name of the offending field.
        field: String,
    },

    /// A trait still carries generic parameters. `MonomorphisePass`
    /// should have specialised every generic-trait impl by now.
    #[error(
        "trait '{trait_name}' still has {param_count} generic parameter(s); MonomorphisePass must run before WasmBackend::generate"
    )]
    GenericTrait {
        /// Name of the offending trait.
        trait_name: String,
        /// Number of unresolved generic parameters.
        param_count: usize,
    },

    /// A [`ResolvedType::TypeParam`] reached the backend, meaning the
    /// module was not fully monomorphised.
    #[error("ResolvedType::TypeParam('{name}') in {location}; module must be monomorphised first")]
    UnresolvedTypeParam {
        /// Name of the unresolved parameter (e.g. `"T"`).
        name: String,
        /// Breadcrumb pointing at the offending location.
        location: String,
    },

    /// A [`ResolvedType::Error`] placeholder reached the backend. The
    /// frontend should have returned its associated `CompilerError`
    /// before invoking the backend.
    #[error(
        "ResolvedType::Error placeholder in {location}; upstream compilation must have failed before reaching the backend"
    )]
    ErrorTypePlaceholder {
        /// Breadcrumb pointing at the offending location.
        location: String,
    },
}

/// Run all pre-flight checks against `module`. Short-circuits on the
/// first violation.
pub fn check(module: &IrModule) -> Result<(), PreflightError> {
    for s in &module.structs {
        check_struct(s)?;
    }
    for t in &module.traits {
        check_trait(t)?;
    }
    for e in &module.enums {
        check_enum(e)?;
    }
    for l in &module.lets {
        check_let(l)?;
    }
    for f in &module.functions {
        check_function(f, &format!("function '{}'", f.name))?;
    }
    for i in &module.impls {
        check_impl(i, module)?;
    }
    Ok(())
}

// ───────────────────────────────────────────────────────────────────
// Item-level checks
// ───────────────────────────────────────────────────────────────────

fn check_struct(s: &IrStruct) -> Result<(), PreflightError> {
    let is_public = matches!(s.visibility, Visibility::Public);
    for field in &s.fields {
        if is_public && matches!(field.ty, ResolvedType::Closure { .. }) {
            return Err(PreflightError::PublicClosureField {
                struct_name: s.name.clone(),
                field: field.name.clone(),
            });
        }
        check_type(
            &field.ty,
            &format!("struct '{}' field '{}'", s.name, field.name),
        )?;
        check_field_default(field, &s.name)?;
    }
    Ok(())
}

fn check_field_default(field: &IrField, owner: &str) -> Result<(), PreflightError> {
    if let Some(default) = &field.default {
        check_expr(default, &format!("default for {owner}::{}", field.name))?;
    }
    Ok(())
}

fn check_trait(t: &IrTrait) -> Result<(), PreflightError> {
    if !t.generic_params.is_empty() {
        return Err(PreflightError::GenericTrait {
            trait_name: t.name.clone(),
            param_count: t.generic_params.len(),
        });
    }
    for field in &t.fields {
        check_type(
            &field.ty,
            &format!("trait '{}' field '{}'", t.name, field.name),
        )?;
    }
    for sig in &t.methods {
        let where_ = format!("trait '{}' method '{}'", t.name, sig.name);
        for p in &sig.params {
            if let Some(ty) = &p.ty {
                check_type(ty, &format!("{where_} parameter '{}'", p.name))?;
            }
        }
        if let Some(rt) = &sig.return_type {
            check_type(rt, &format!("{where_} return type"))?;
        }
    }
    Ok(())
}

fn check_enum(e: &formalang::ir::IrEnum) -> Result<(), PreflightError> {
    for variant in &e.variants {
        for field in &variant.fields {
            check_type(
                &field.ty,
                &format!(
                    "enum '{}' variant '{}' field '{}'",
                    e.name, variant.name, field.name
                ),
            )?;
        }
    }
    Ok(())
}

fn check_let(l: &IrLet) -> Result<(), PreflightError> {
    let location = format!("let '{}'", l.name);
    check_type(&l.ty, &location)?;
    check_expr(&l.value, &location)?;
    Ok(())
}

fn check_function(f: &IrFunction, location: &str) -> Result<(), PreflightError> {
    for p in &f.params {
        if let Some(ty) = &p.ty {
            check_type(ty, &format!("{location} parameter '{}'", p.name))?;
        }
    }
    if let Some(rt) = &f.return_type {
        check_type(rt, &format!("{location} return type"))?;
    }
    if let Some(body) = &f.body {
        check_expr(body, &format!("{location} body"))?;
    }
    Ok(())
}

fn check_impl(i: &IrImpl, module: &IrModule) -> Result<(), PreflightError> {
    let target_name = match i.target {
        formalang::ir::ImplTarget::Struct(id) => module
            .get_struct(id)
            .map_or_else(|| format!("struct#{}", id.0), |s| s.name.clone()),
        formalang::ir::ImplTarget::Enum(id) => module
            .get_enum(id)
            .map_or_else(|| format!("enum#{}", id.0), |e| e.name.clone()),
        formalang::ir::ImplTarget::Primitive(p) => format!("primitive {p:?}"),
    };
    for f in &i.functions {
        check_function(f, &format!("impl '{target_name}' method '{}'", f.name))?;
    }
    Ok(())
}

// ───────────────────────────────────────────────────────────────────
// Type walker
// ───────────────────────────────────────────────────────────────────

fn check_type(ty: &ResolvedType, location: &str) -> Result<(), PreflightError> {
    match ty {
        ResolvedType::Primitive(_)
        | ResolvedType::Struct(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_) => Ok(()),

        ResolvedType::Array(inner) | ResolvedType::Range(inner) | ResolvedType::Optional(inner) => {
            check_type(inner, location)
        }

        ResolvedType::Tuple(fields) => {
            for (_, inner) in fields {
                check_type(inner, location)?;
            }
            Ok(())
        }

        ResolvedType::Generic { args, .. } => {
            for arg in args {
                check_type(arg, location)?;
            }
            Ok(())
        }

        ResolvedType::External { type_args, .. } => {
            for arg in type_args {
                check_type(arg, location)?;
            }
            Ok(())
        }

        ResolvedType::Dictionary { key_ty, value_ty } => {
            check_type(key_ty, location)?;
            check_type(value_ty, location)
        }

        ResolvedType::Closure {
            param_tys,
            return_ty,
        } => {
            for (_, inner) in param_tys {
                check_type(inner, location)?;
            }
            check_type(return_ty, location)
        }

        ResolvedType::TypeParam(name) => Err(PreflightError::UnresolvedTypeParam {
            name: name.clone(),
            location: location.to_owned(),
        }),

        ResolvedType::Error => Err(PreflightError::ErrorTypePlaceholder {
            location: location.to_owned(),
        }),
    }
}

// ───────────────────────────────────────────────────────────────────
// Expression walker
// ───────────────────────────────────────────────────────────────────

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive walk over every IrExpr variant; splitting hides the per-variant pre-flight contract"
)]
fn check_expr(expr: &IrExpr, location: &str) -> Result<(), PreflightError> {
    check_type(expr.ty(), location)?;

    match expr {
        IrExpr::Closure { .. } => Err(PreflightError::UnconvertedClosure {
            location: location.to_owned(),
        }),

        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::LetRef { .. } => Ok(()),

        IrExpr::StructInst { fields, .. } | IrExpr::EnumInst { fields, .. } => {
            for (_, _, sub) in fields {
                check_expr(sub, location)?;
            }
            Ok(())
        }

        IrExpr::Tuple { fields, .. } => {
            for (_, sub) in fields {
                check_expr(sub, location)?;
            }
            Ok(())
        }

        IrExpr::Array { elements, .. } => {
            for e in elements {
                check_expr(e, location)?;
            }
            Ok(())
        }

        IrExpr::FieldAccess { object, .. } => check_expr(object, location),

        IrExpr::BinaryOp { left, right, .. } => {
            check_expr(left, location)?;
            check_expr(right, location)
        }

        IrExpr::UnaryOp { operand, .. } => check_expr(operand, location),

        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            check_expr(condition, location)?;
            check_expr(then_branch, location)?;
            if let Some(else_branch) = else_branch {
                check_expr(else_branch, location)?;
            }
            Ok(())
        }

        IrExpr::For {
            collection, body, ..
        } => {
            check_expr(collection, location)?;
            check_expr(body, location)
        }

        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            check_expr(scrutinee, location)?;
            for arm in arms {
                check_match_arm(arm, location)?;
            }
            Ok(())
        }

        IrExpr::FunctionCall { args, .. } => {
            for (_, sub) in args {
                check_expr(sub, location)?;
            }
            Ok(())
        }

        IrExpr::CallClosure { closure, args, .. } => {
            check_expr(closure, location)?;
            for (_, sub) in args {
                check_expr(sub, location)?;
            }
            Ok(())
        }

        IrExpr::MethodCall { receiver, args, .. } => {
            check_expr(receiver, location)?;
            for (_, sub) in args {
                check_expr(sub, location)?;
            }
            Ok(())
        }

        IrExpr::ClosureRef { env_struct, .. } => check_expr(env_struct, location),

        IrExpr::DictLiteral { entries, .. } => {
            for (k, v) in entries {
                check_expr(k, location)?;
                check_expr(v, location)?;
            }
            Ok(())
        }

        IrExpr::DictAccess { dict, key, .. } => {
            check_expr(dict, location)?;
            check_expr(key, location)
        }

        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                check_block_statement(stmt, location)?;
            }
            check_expr(result, location)
        }
    }
}

fn check_block_statement(stmt: &IrBlockStatement, location: &str) -> Result<(), PreflightError> {
    match stmt {
        IrBlockStatement::Let { ty, value, .. } => {
            if let Some(ty) = ty {
                check_type(ty, location)?;
            }
            check_expr(value, location)
        }
        IrBlockStatement::Assign { target, value, .. } => {
            check_expr(target, location)?;
            check_expr(value, location)
        }
        IrBlockStatement::Expr(e) => check_expr(e, location),
    }
}

fn check_match_arm(arm: &IrMatchArm, location: &str) -> Result<(), PreflightError> {
    for (_, _, ty) in &arm.bindings {
        check_type(ty, location)?;
    }
    check_expr(&arm.body, location)
}
