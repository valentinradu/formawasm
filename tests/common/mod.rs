//! Test fixtures shared across hand-built-IR tests.
//!
//! formalang 0.0.4-beta collapsed `Optional<T>` / `Array<T>` /
//! `Range<T>` / `Dictionary<K, V>` from explicit `ResolvedType`
//! variants into `Generic { base, args }` matched against
//! `IrModule::prelude_*_id()`. Hand-built test IRs that don't go
//! through `compile_to_ir` (so the prelude isn't auto-attached) need
//! to seed the four prelude defs themselves; this module is the
//! shared seeder.
//!
//! Convention: every test that constructs an `IrModule` by hand
//! calls [`seed_prelude`] *before* pushing any user struct/enum.
//! That way the prelude entries occupy the leading IDs in
//! `module.structs` / `module.enums` (Array=0, Dictionary=1,
//! Range=2 / Optional=0). The helper constructors below hard-code
//! those IDs so call sites read like the old API
//! (`array_ty(elem)` rather than `array_ty(elem, &ids)`).
//!
//! Usage:
//!
//! ```ignore
//! mod common;
//! use common::{seed_prelude, array_ty, optional_ty};
//!
//! let mut module = IrModule::new();
//! seed_prelude(&mut module);
//! let arr_i32 = array_ty(primitive(I32));
//! ```

use formalang::ast::{ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{
    EnumId, GenericBase, IrEnum, IrEnumVariant, IrField, IrGenericParam, IrModule, IrSpan,
    IrStruct, ResolvedType, StructId,
};

/// Smoke test that every public helper in this module compiles
/// and runs. Doubles as a dead-code suppressor: each test target
/// invokes `mod common`, and without this single test referencing
/// every helper, dead-code warnings would flicker across the
/// per-target compile matrix (each test file uses a different
/// subset of helpers).
#[test]
fn _force_use_helpers() {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let _ = optional_ty(ResolvedType::Primitive(PrimitiveType::I32));
    let _ = array_ty(ResolvedType::Primitive(PrimitiveType::I32));
    let _ = range_ty(ResolvedType::Primitive(PrimitiveType::I32));
    let _ = dict_ty(
        ResolvedType::Primitive(PrimitiveType::String),
        ResolvedType::Primitive(PrimitiveType::I32),
    );
    let _ = PRELUDE_ARRAY_ID;
    let _ = PRELUDE_DICTIONARY_ID;
    let _ = PRELUDE_RANGE_ID;
    let _ = PRELUDE_OPTIONAL_ID;
}

/// `StructId` the seeder assigns to `Array<T>` (0). Hand-built
/// helpers below hard-code this assuming `seed_prelude` ran first.
pub(crate) const PRELUDE_ARRAY_ID: StructId = StructId(0);
/// `StructId` the seeder assigns to `Dictionary<K, V>` (1).
pub(crate) const PRELUDE_DICTIONARY_ID: StructId = StructId(1);
/// `StructId` the seeder assigns to `Range<T>` (2).
pub(crate) const PRELUDE_RANGE_ID: StructId = StructId(2);
/// `EnumId` the seeder assigns to `Optional<T>` (0).
pub(crate) const PRELUDE_OPTIONAL_ID: EnumId = EnumId(0);

/// Seed `module` with declarations for the four prelude built-ins
/// (`Array<T>`, `Dictionary<K, V>`, `Range<T>`, `Optional<T>`), then
/// rebuild the name→id indices so the `prelude_*_id()` accessors
/// resolve. The Optional declaration carries `[some(value: T), none]`
/// to mirror the actual prelude — the canonical-ABI wrapper plans
/// the enum layout from this so an empty-variant placeholder would
/// trip [`crate::layout::LayoutError::UninhabitedEnum`].
///
/// **Call this before pushing any user struct/enum** so the
/// hard-coded ids in [`PRELUDE_ARRAY_ID`] / etc. line up.
pub(crate) fn seed_prelude(module: &mut IrModule) {
    module.structs.push(empty_generic_struct("Array"));
    module.structs.push(empty_generic_struct("Dictionary"));
    module.structs.push(empty_generic_struct("Range"));
    module.enums.push(prelude_optional_enum());
    module.rebuild_indices();
}

fn empty_generic_struct(name: &str) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields: Vec::new(),
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn prelude_optional_enum() -> IrEnum {
    IrEnum {
        name: "Optional".to_owned(),
        visibility: Visibility::Public,
        variants: vec![
            IrEnumVariant {
                name: "some".to_owned(),
                fields: vec![IrField {
                    name: "value".to_owned(),
                    ty: ResolvedType::TypeParam("T".to_owned()),
                    mutable: false,
                    optional: false,
                    default: None,
                    doc: None,
                    convention: ParamConvention::Let,
                    span: IrSpan::default(),
                }],
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "none".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
        ],
        generic_params: vec![IrGenericParam {
            name: "T".to_owned(),
            constraints: Vec::new(),
        }],
        doc: None,
        span: IrSpan::default(),
    }
}

// Convenience constructors for the four compound shapes — each
// returns the canonical `Generic { base, args }` form keyed against
// the prelude IDs the seeder commits to.

/// `Optional<inner>` — assumes [`seed_prelude`] has been called on
/// the `IrModule` the resulting type will be paired with.
#[must_use]
pub(crate) fn optional_ty(inner: ResolvedType) -> ResolvedType {
    ResolvedType::Generic {
        base: GenericBase::Enum(PRELUDE_OPTIONAL_ID),
        args: vec![inner],
    }
}

/// `Array<elem>` — assumes [`seed_prelude`] has been called.
#[must_use]
pub(crate) fn array_ty(elem: ResolvedType) -> ResolvedType {
    ResolvedType::Generic {
        base: GenericBase::Struct(PRELUDE_ARRAY_ID),
        args: vec![elem],
    }
}

/// `Range<bound>` — assumes [`seed_prelude`] has been called.
#[must_use]
pub(crate) fn range_ty(bound: ResolvedType) -> ResolvedType {
    ResolvedType::Generic {
        base: GenericBase::Struct(PRELUDE_RANGE_ID),
        args: vec![bound],
    }
}

/// `Dictionary<key, value>` — assumes [`seed_prelude`] has been
/// called.
#[must_use]
pub(crate) fn dict_ty(key: ResolvedType, value: ResolvedType) -> ResolvedType {
    ResolvedType::Generic {
        base: GenericBase::Struct(PRELUDE_DICTIONARY_ID),
        args: vec![key, value],
    }
}
