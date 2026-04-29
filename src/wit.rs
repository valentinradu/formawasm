//! WIT-document generation for the component boundary.
//!
//! Turns the `(IrModule, PublicSurface)` pair into a WIT source string
//! describing one world named `component`, with one `export` per
//! exported function. Phase 1a covers primitive-only signatures —
//! aggregate types (struct, enum, tuple, array, …) surface here as
//! [`TypeMapError::NotYetSupported`] (wrapped in
//! [`WitEmitError::TypeMap`]) until their lowerings land.
//!
//! The emitted WIT is parsed back through [`wit_parser::Resolve`]
//! before returning, so a string this module yields is guaranteed to
//! be syntactically valid WIT and to round-trip to a single world.

use std::fmt::Write as _;

use formalang::ast::PrimitiveType;
use formalang::ir::{IrEnum, IrFunction, IrModule, IrStruct, ResolvedType};
use thiserror::Error;
use wit_parser::Resolve;

use crate::survey::PublicSurface;
use crate::types::TypeMapError;

/// Hard-coded WIT package identifier for components emitted by this backend.
///
/// Formalang has no notion of a published package namespace yet, so every
/// generated component lives under the same synthetic package.
pub const PACKAGE: &str = "formawasm:generated";

/// Hard-coded world name for the emitted component.
///
/// One world per component is enough for Phase 1a; partitioning a
/// public surface into multiple worlds is out of scope until cross-
/// module imports land in Phase 4.
pub const WORLD_NAME: &str = "component";

/// Errors produced by [`emit_wit`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WitEmitError {
    /// A type appearing in a public function signature has no WIT
    /// counterpart in this phase — typically because the lowering for
    /// it lives in a later phase, or because it is not WIT-expressible
    /// at all (e.g. closure types).
    #[error(transparent)]
    TypeMap(#[from] TypeMapError),

    /// A `Never`-typed parameter slipped through. The frontend should
    /// have rejected it long before reaching us; the variant exists so
    /// pre-flight bugs don't produce unsafe WIT.
    #[error("parameter '{param}' on function '{function}' has type Never")]
    NeverParam {
        /// Containing function name.
        function: String,
        /// Parameter name.
        param: String,
    },

    /// A function parameter is missing its type annotation. Only `self`
    /// parameters legitimately omit the type, and Phase 1a does not yet
    /// emit methods.
    #[error("parameter '{param}' on function '{function}' is missing a type annotation")]
    MissingParamType {
        /// Containing function name.
        function: String,
        /// Parameter name.
        param: String,
    },

    /// `surface.exports` references a `FunctionId` past the end of
    /// `module.functions`. Indicates the surface and module disagree on
    /// indexing — almost always a caller bug.
    #[error("PublicSurface references function index {index} but module has only {len}")]
    ExportOutOfRange {
        /// The offending index.
        index: u32,
        /// Number of functions in the module.
        len: usize,
    },

    /// The WIT string we built failed to parse via `wit-parser`. The
    /// underlying error is wrapped as a string because `wit-parser`
    /// surfaces its diagnostics through `anyhow::Error`, which is not
    /// `Sync` and cannot be propagated through `thiserror`'s `#[from]`.
    #[error("emitted WIT failed to parse: {reason}")]
    Invalid {
        /// `wit-parser`'s error rendered as a string.
        reason: String,
    },
}

/// Build the WIT source string for `module`'s public surface.
///
/// The result is the contents of a single `.wit` file containing one
/// `package` line, one `record` per public struct in
/// `surface.exported_structs`, one `variant` per public enum in
/// `surface.exported_enums`, and one `world` declaration with one
/// `export` per function in `surface.exports`. Imports surface in
/// Phase 4.
pub fn emit_wit(module: &IrModule, surface: &PublicSurface) -> Result<String, WitEmitError> {
    let mut out = String::new();
    writeln!(out, "package {PACKAGE};").map_err(invalid_format)?;
    writeln!(out).map_err(invalid_format)?;
    let mut type_names: Vec<String> = Vec::new();
    let any_types = !surface.exported_structs.is_empty() || !surface.exported_enums.is_empty();
    if any_types {
        writeln!(out, "interface types {{").map_err(invalid_format)?;
        for &sid in &surface.exported_structs {
            let s = module
                .structs
                .get(sid.0 as usize)
                .ok_or(WitEmitError::ExportOutOfRange {
                    index: sid.0,
                    len: module.structs.len(),
                })?;
            write_record(&mut out, s)?;
            type_names.push(kebab_case(&s.name));
        }
        for &eid in &surface.exported_enums {
            let e = module
                .enums
                .get(eid.0 as usize)
                .ok_or(WitEmitError::ExportOutOfRange {
                    index: eid.0,
                    len: module.enums.len(),
                })?;
            write_variant(&mut out, e)?;
            type_names.push(kebab_case(&e.name));
        }
        writeln!(out, "}}").map_err(invalid_format)?;
        writeln!(out).map_err(invalid_format)?;
    }
    writeln!(out, "world {WORLD_NAME} {{").map_err(invalid_format)?;
    if !type_names.is_empty() {
        write!(out, "  use types.{{").map_err(invalid_format)?;
        for (i, n) in type_names.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(n);
        }
        writeln!(out, "}};").map_err(invalid_format)?;
    }

    for &fid in &surface.exports {
        let f = module
            .functions
            .get(fid.0 as usize)
            .ok_or(WitEmitError::ExportOutOfRange {
                index: fid.0,
                len: module.functions.len(),
            })?;
        write_export(&mut out, f)?;
    }

    writeln!(out, "}}").map_err(invalid_format)?;

    parse_round_trip(&out)?;
    Ok(out)
}

fn write_record(out: &mut String, s: &IrStruct) -> Result<(), WitEmitError> {
    writeln!(out, "  record {} {{", kebab_case(&s.name)).map_err(invalid_format)?;
    for f in &s.fields {
        let wit_ty = resolved_wit_type(&f.ty)?.ok_or_else(|| WitEmitError::NeverParam {
            function: format!("record {}", s.name),
            param: f.name.clone(),
        })?;
        writeln!(out, "    {}: {wit_ty},", kebab_case(&f.name)).map_err(invalid_format)?;
    }
    writeln!(out, "  }}").map_err(invalid_format)?;
    Ok(())
}

fn write_variant(out: &mut String, e: &IrEnum) -> Result<(), WitEmitError> {
    writeln!(out, "  variant {} {{", kebab_case(&e.name)).map_err(invalid_format)?;
    for v in &e.variants {
        match v.fields.len() {
            0 => {
                writeln!(out, "    {},", kebab_case(&v.name)).map_err(invalid_format)?;
            }
            1 => {
                let f = v.fields.first().ok_or_else(|| WitEmitError::NeverParam {
                    function: format!("variant {}", e.name),
                    param: "(0-th field)".to_owned(),
                })?;
                let wit_ty = resolved_wit_type(&f.ty)?.ok_or_else(|| WitEmitError::NeverParam {
                    function: format!("variant {}", e.name),
                    param: f.name.clone(),
                })?;
                writeln!(out, "    {}({wit_ty}),", kebab_case(&v.name)).map_err(invalid_format)?;
            }
            _ => {
                return Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: format!("variant {} payload with > 1 field", v.name),
                }));
            }
        }
    }
    writeln!(out, "  }}").map_err(invalid_format)?;
    Ok(())
}

/// Convert a name to lower-kebab-case suitable as a WIT identifier.
/// Splits on existing case transitions: `MyType` → `my-type`,
/// `Pair` → `pair`, `XMLParser` → `x-m-l-parser`.
fn kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
        } else if ch == '_' {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn write_export(out: &mut String, f: &IrFunction) -> Result<(), WitEmitError> {
    write!(out, "  export {}: func(", f.name).map_err(invalid_format)?;
    for (i, param) in f.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let ty = param
            .ty
            .as_ref()
            .ok_or_else(|| WitEmitError::MissingParamType {
                function: f.name.clone(),
                param: param.name.clone(),
            })?;
        let wit_ty = resolved_wit_type(ty)?.ok_or_else(|| WitEmitError::NeverParam {
            function: f.name.clone(),
            param: param.name.clone(),
        })?;
        write!(out, "{}: {}", param.name, wit_ty).map_err(invalid_format)?;
    }
    write!(out, ")").map_err(invalid_format)?;
    if let Some(ret) = f.return_type.as_ref()
        && let Some(wit_ty) = resolved_wit_type(ret)?
    {
        write!(out, " -> {wit_ty}").map_err(invalid_format)?;
    }
    writeln!(out, ";").map_err(invalid_format)?;
    Ok(())
}

/// Map a [`ResolvedType`] to the WIT type name string used inside a
/// `func(...)` signature, or `Ok(None)` for `Never` (no value, omit
/// the result clause when this is a return type — caller's choice).
fn resolved_wit_type(ty: &ResolvedType) -> Result<Option<&'static str>, WitEmitError> {
    let ResolvedType::Primitive(p) = ty else {
        return Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
            kind: variant_tag(ty),
        }));
    };
    primitive_wit_type(*p)
}

/// Map a [`PrimitiveType`] to its WIT type name. `Never` returns
/// `None` so callers can decide between rejecting it (parameters) or
/// omitting the result clause (returns).
fn primitive_wit_type(p: PrimitiveType) -> Result<Option<&'static str>, WitEmitError> {
    match p {
        PrimitiveType::I32 => Ok(Some("s32")),
        PrimitiveType::I64 => Ok(Some("s64")),
        PrimitiveType::F32 => Ok(Some("f32")),
        PrimitiveType::F64 => Ok(Some("f64")),
        PrimitiveType::Boolean => Ok(Some("bool")),
        PrimitiveType::Never => Ok(None),
        PrimitiveType::String | PrimitiveType::Path | PrimitiveType::Regex => {
            Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                kind: format!("{p:?}"),
            }))
        }
        _ => Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("{p:?}"),
        })),
    }
}

/// String tag for non-primitive `ResolvedType` variants used in the
/// `NotYetSupported` diagnostic. Mirrors the tags chosen in
/// [`crate::types::resolved_value_type`] so messages stay consistent
/// across the two surfaces.
fn variant_tag(ty: &ResolvedType) -> String {
    match ty {
        ResolvedType::Primitive(p) => format!("{p:?}"),
        ResolvedType::Struct(_) => "Struct".to_owned(),
        ResolvedType::Trait(_) => "Trait".to_owned(),
        ResolvedType::Enum(_) => "Enum".to_owned(),
        ResolvedType::Array(_) => "Array<T>".to_owned(),
        ResolvedType::Range(_) => "Range<T>".to_owned(),
        ResolvedType::Optional(_) => "Optional<T>".to_owned(),
        ResolvedType::Tuple(_) => "Tuple".to_owned(),
        ResolvedType::Generic { .. } => "Generic".to_owned(),
        ResolvedType::TypeParam(name) => format!("TypeParam({name})"),
        ResolvedType::External { name, .. } => format!("External({name})"),
        ResolvedType::Dictionary { .. } => "Dictionary<K, V>".to_owned(),
        ResolvedType::Closure { .. } => "Closure".to_owned(),
        ResolvedType::Error => "Error".to_owned(),
    }
}

fn parse_round_trip(wit: &str) -> Result<(), WitEmitError> {
    let mut resolve = Resolve::default();
    let pkg = resolve
        .push_str("formawasm-generated.wit", wit)
        .map_err(|e| WitEmitError::Invalid {
            reason: format!("{e:#}"),
        })?;
    resolve
        .select_world(&[pkg], Some(WORLD_NAME))
        .map_err(|e| WitEmitError::Invalid {
            reason: format!("{e:#}"),
        })?;
    Ok(())
}

/// Bridge `std::fmt::Error` (writes into `String` are infallible in
/// practice) to a `WitEmitError::Invalid` so the function signature
/// stays single-error.
fn invalid_format(e: std::fmt::Error) -> WitEmitError {
    WitEmitError::Invalid {
        reason: e.to_string(),
    }
}
