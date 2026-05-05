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

use formalang::ir::{GenericBase, IrModule, ResolvedType};

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
            GenericBase::Enum(eid) if Some(*eid) == module.prelude_optional_id() => args
                .first()
                .map_or(Self::None, Self::Optional),
            GenericBase::Struct(sid) if Some(*sid) == module.prelude_array_id() => args
                .first()
                .map_or(Self::None, Self::Array),
            GenericBase::Struct(sid) if Some(*sid) == module.prelude_range_id() => args
                .first()
                .map_or(Self::None, Self::Range),
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
pub(crate) fn array_elem<'a>(
    ty: &'a ResolvedType,
    module: &IrModule,
) -> Option<&'a ResolvedType> {
    match Compound::of(ty, module) {
        Compound::Array(t) => Some(t),
        _ => None,
    }
}

/// Convenience: extract `Range<T>`'s bound type, or `None`.
pub(crate) fn range_bound<'a>(
    ty: &'a ResolvedType,
    module: &IrModule,
) -> Option<&'a ResolvedType> {
    match Compound::of(ty, module) {
        Compound::Range(t) => Some(t),
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
