//! Recognise the four prelude compound types under the new
//! `ResolvedType::Generic { base, args }` representation.
//!
//! formalang 0.0.4-beta collapsed `Optional<T>`, `Array<T>`,
//! `Range<T>` and `Dictionary<K, V>` from explicit `ResolvedType`
//! variants into a single generic shape whose base ID matches one
//! of `IrModule`'s `prelude_*_id()` accessors. Backends classify a
//! generic by its base ID; everything else (user generics) flows
//! through the existing `ResolvedType::Generic` arm.
//!
//! [`Compound`] is a small view enum: ask `Compound::of(ty, module)`
//! at any match site and dispatch on the result instead of duplicating
//! the prelude-ID lookup at each call site.
//!
//! All four constructors carry borrowed inner types so callers don't
//! pay an allocation per recognition.

use formalang::ir::{
    EnumId, GenericBase, IrEnum, IrField, IrGenericParam, IrModule, ResolvedType, StructId,
};

/// View of a `ResolvedType` that resolves the prelude compounds.
///
/// Returned by [`Compound::of`]. Variants beyond the four prelude
/// shapes don't appear here; pattern-match `Compound::None` and
/// fall through to the original `ResolvedType` arms in that case.
#[derive(Debug)]
pub(crate) enum Compound<'a> {
    /// `Optional<T>` — desugared from `T?` and the `nil` literal.
    Optional(&'a ResolvedType),
    /// `Array<T>` — desugared from `[T]` and the `[a, b, c]` literal.
    Array(&'a ResolvedType),
    /// `Range<T>` — desugared from `start..end`.
    Range(&'a ResolvedType),
    /// `Dictionary<K, V>` — desugared from `[K: V]` and the
    /// `["k": v]` literal.
    Dictionary {
        /// Key type.
        key: &'a ResolvedType,
        /// Value type.
        value: &'a ResolvedType,
    },
    /// Anything else: not a prelude compound.
    None,
}

impl<'a> Compound<'a> {
    /// Classify `ty` against the prelude IDs in `module`.
    ///
    /// Non-`Generic` shapes return [`Compound::None`] without any
    /// allocation. A `Generic` whose base doesn't match any prelude
    /// ID also returns `None` — that's a user-defined generic.
    pub(crate) fn of(ty: &'a ResolvedType, module: &IrModule) -> Self {
        let ResolvedType::Generic { base, args } = ty else {
            return Self::None;
        };
        match base {
            GenericBase::Enum(eid) if Some(*eid) == module.prelude_optional_id() => {
                args.first().map_or(Self::None, Self::Optional)
            }
            GenericBase::Struct(sid) if Some(*sid) == module.prelude_array_id() => {
                args.first().map_or(Self::None, Self::Array)
            }
            GenericBase::Struct(sid) if Some(*sid) == module.prelude_range_id() => {
                args.first().map_or(Self::None, Self::Range)
            }
            GenericBase::Struct(sid) if Some(*sid) == module.prelude_dictionary_id() => {
                if let [k, v] = args.as_slice() {
                    Self::Dictionary { key: k, value: v }
                } else {
                    Self::None
                }
            }
            _ => Self::None,
        }
    }
}

/// Convenience: extract `Optional<T>`'s inner type, or `None`.
pub(crate) fn optional_inner<'a>(
    ty: &'a ResolvedType,
    module: &IrModule,
) -> Option<&'a ResolvedType> {
    match Compound::of(ty, module) {
        Compound::Optional(t) => Some(t),
        _ => None,
    }
}

/// Convenience: extract `Array<T>`'s element type, or `None`.
pub(crate) fn array_elem<'a>(ty: &'a ResolvedType, module: &IrModule) -> Option<&'a ResolvedType> {
    match Compound::of(ty, module) {
        Compound::Array(t) => Some(t),
        _ => None,
    }
}

/// Convenience: extract `Range<T>`'s bound type, or `None`.
pub(crate) fn range_bound<'a>(ty: &'a ResolvedType, module: &IrModule) -> Option<&'a ResolvedType> {
    match Compound::of(ty, module) {
        Compound::Range(t) => Some(t),
        _ => None,
    }
}

/// Resolve a `ResolvedType` to its underlying [`EnumId`], peeking
/// through `Generic { base: Enum(_) }` (the post-0.0.4-beta shape
/// for `Optional<T>` and any user enum monomorphisation produces).
/// Returns `None` for non-enum types.
pub(crate) fn enum_id_of(ty: &ResolvedType) -> Option<EnumId> {
    match ty {
        ResolvedType::Enum(id) => Some(*id),
        ResolvedType::Generic {
            base: GenericBase::Enum(id),
            ..
        } => Some(*id),
        _ => None,
    }
}

/// Substitute every `TypeParam(name)` inside `ty` with the matching
/// generic argument from `(generic_params, type_args)`. Walks the
/// type tree and rebuilds it; preserves shape for any
/// `TypeParam(_)` whose name doesn't appear in `generic_params`
/// (returns it unchanged so the caller can decide what to do).
///
/// Used by the layout planner so a `Generic { base: Enum(opt), args:
/// [String] }` can produce a substituted `IrEnum` whose variant
/// fields carry concrete types (`String`) instead of the
/// declaration's `TypeParam("T")` placeholder.
pub(crate) fn substitute_type_params(
    ty: &ResolvedType,
    generic_params: &[IrGenericParam],
    type_args: &[ResolvedType],
) -> ResolvedType {
    match ty {
        ResolvedType::TypeParam(name) => {
            if let Some(idx) = generic_params.iter().position(|p| &p.name == name)
                && let Some(arg) = type_args.get(idx)
            {
                return arg.clone();
            }
            ty.clone()
        }
        ResolvedType::Generic { base, args } => ResolvedType::Generic {
            base: base.clone(),
            args: args
                .iter()
                .map(|a| substitute_type_params(a, generic_params, type_args))
                .collect(),
        },
        ResolvedType::Tuple(fields) => ResolvedType::Tuple(
            fields
                .iter()
                .map(|(n, t)| {
                    (
                        n.clone(),
                        substitute_type_params(t, generic_params, type_args),
                    )
                })
                .collect(),
        ),
        ResolvedType::External { .. } => ty.clone(),
        ResolvedType::Closure {
            param_tys,
            return_ty,
        } => ResolvedType::Closure {
            param_tys: param_tys
                .iter()
                .map(|(c, t)| (*c, substitute_type_params(t, generic_params, type_args)))
                .collect(),
            return_ty: Box::new(substitute_type_params(return_ty, generic_params, type_args)),
        },
        ResolvedType::Primitive(_)
        | ResolvedType::Struct(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Error => ty.clone(),
    }
}

/// Build a substituted [`IrEnum`] whose variant fields have every
/// `TypeParam(name)` replaced by the corresponding entry in
/// `type_args` (matched against `generic_params` by name). Returns
/// the original enum unchanged when the enum has no generic
/// parameters or `type_args` is empty.
pub(crate) fn substitute_enum(
    e: &IrEnum,
    generic_params: &[IrGenericParam],
    type_args: &[ResolvedType],
) -> IrEnum {
    if generic_params.is_empty() || type_args.is_empty() {
        return e.clone();
    }
    let mut clone = e.clone();
    for variant in &mut clone.variants {
        for field in &mut variant.fields {
            field.ty = substitute_type_params(&field.ty, generic_params, type_args);
        }
    }
    // The substituted enum is a concrete monomorphisation, not a
    // generic declaration. Drop the generic-params list so the
    // preflight / wit emitter don't treat it as polymorphic.
    clone.generic_params.clear();
    clone
}

/// Pull the generic args out of a `ResolvedType::Generic` whose base
/// matches `expected_id` (as either `Enum` or `Struct`). Returns
/// `&[]` for any other shape so callers can pass it straight to
/// [`substitute_enum`] / similar without conditional branching.
pub(crate) fn generic_args_for_enum<'a>(
    ty: &'a ResolvedType,
    expected_id: EnumId,
) -> &'a [ResolvedType] {
    if let ResolvedType::Generic {
        base: GenericBase::Enum(id),
        args,
    } = ty
        && *id == expected_id
    {
        args.as_slice()
    } else {
        &[]
    }
}

/// Borrow shim so `substitute_enum_field_types` can plug into a
/// match-arm payload-binding loop without re-allocating the field
/// vector when no substitution applies.
#[must_use]
pub(crate) fn substitute_field_ty<'a>(
    field: &'a IrField,
    generic_params: &[IrGenericParam],
    type_args: &[ResolvedType],
) -> ResolvedType {
    if generic_params.is_empty() || type_args.is_empty() {
        return field.ty.clone();
    }
    substitute_type_params(&field.ty, generic_params, type_args)
}

/// Resolve a `ResolvedType` to its underlying [`StructId`], peeking
/// through `Generic { base: Struct(_) }`. Returns `None` for non-
/// struct types.
pub(crate) fn struct_id_of(ty: &ResolvedType) -> Option<StructId> {
    match ty {
        ResolvedType::Struct(id) => Some(*id),
        ResolvedType::Generic {
            base: GenericBase::Struct(id),
            ..
        } => Some(*id),
        _ => None,
    }
}

/// Convenience: extract `Dictionary<K, V>`'s key/value types, or
/// `None`.
pub(crate) fn dictionary_kv<'a>(
    ty: &'a ResolvedType,
    module: &IrModule,
) -> Option<(&'a ResolvedType, &'a ResolvedType)> {
    match Compound::of(ty, module) {
        Compound::Dictionary { key, value } => Some((key, value)),
        _ => None,
    }
}
