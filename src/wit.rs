//! WIT-document generation for the component boundary.
//!
//! Turns the `(IrModule, PublicSurface)` pair into a WIT source string
//! describing one world (named `component`) carrying one `import`
//! line per `extern_abi`-bearing function in `surface.imports` and
//! one `export` line per non-extern function in `surface.exports`,
//! plus a sibling `interface types {}` block for every public struct
//! / enum the world references.
//!
//! Type coverage on the boundary:
//! - Primitives (`I32` / `I64` / `F32` / `F64` / `Boolean`,
//!   `String` / `Path` / `Regex` as WIT `string`).
//! - `Optional<T>` → `option<T>`, `Array<T>` → `list<T>`,
//!   `Dictionary<K, V>` → `list<tuple<K, V>>`.
//! - `IrStruct` → `record { ... }`,
//!   `IrEnum` → `variant { ... }` with multi-field payloads
//!   emitted as `tuple<T0, T1, ...>` arms (positional — field
//!   names don't survive the boundary).
//!
//! `External` stays rejected with a breadcrumb pointing at the
//! upstream cross-module-codegen design note. Closures, ranges,
//! generics, type-params, and unit tuples are not WIT-expressible
//! and surface as [`WitEmitError::TypeMap`].
//!
//! The emitted WIT is parsed back through [`wit_parser::Resolve`]
//! before returning, so a string this module yields is guaranteed to
//! be syntactically valid WIT and to round-trip to a single world.

use std::fmt::Write as _;

use formalang::ast::PrimitiveType;
use formalang::ir::{IrEnum, IrFunction, IrModule, IrStruct, ResolvedType};
use thiserror::Error;
use wit_parser::Resolve;

use crate::compound::Compound;
use crate::ident::kebab_case;
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
/// `surface.exported_enums`, and one `world` declaration carrying:
/// one `import` per `extern_abi`-bearing function in
/// `surface.imports` and one `export` per function in
/// `surface.exports`. Cross-module references via
/// `ResolvedType::External` follow in Phase 4 mc3.
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
            write_record(&mut out, s, module)?;
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
            write_variant(&mut out, e, module)?;
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

    for &fid in &surface.imports {
        let f = module
            .functions
            .get(fid.0 as usize)
            .ok_or(WitEmitError::ExportOutOfRange {
                index: fid.0,
                len: module.functions.len(),
            })?;
        write_import(&mut out, f, module)?;
    }

    // `extern impl <UserStruct>` / `<UserEnum>` blocks: each method
    // is a host-supplied function. Emit a WIT `import` per method
    // using the `<struct>-<method>` kebab-case name the
    // module_lowering import phase commits to. Skip primitive
    // targets — the prelude's `extern impl <String>` block is
    // satisfied by built-in runtime helpers, not host imports.
    for imp in &module.impls {
        if !imp.is_extern {
            continue;
        }
        // Skip prelude built-ins; the backend's runtime helpers
        // resolve those — they don't cross the component import
        // boundary. Mirrors the filter in
        // `module_lowering::lower_module`.
        let is_prelude_target = match imp.target {
            formalang::ir::ImplTarget::Struct(sid) => {
                Some(sid) == module.prelude_array_id()
                    || Some(sid) == module.prelude_dictionary_id()
                    || Some(sid) == module.prelude_range_id()
            }
            formalang::ir::ImplTarget::Enum(eid) => Some(eid) == module.prelude_optional_id(),
            formalang::ir::ImplTarget::Primitive(_) => true,
        };
        if is_prelude_target {
            continue;
        }
        let target_name: Option<&str> = match &imp.target {
            formalang::ir::ImplTarget::Struct(sid) => {
                module.structs.get(sid.0 as usize).map(|s| s.name.as_str())
            }
            formalang::ir::ImplTarget::Enum(eid) => {
                module.enums.get(eid.0 as usize).map(|e| e.name.as_str())
            }
            formalang::ir::ImplTarget::Primitive(_) => None,
        };
        let Some(target_name) = target_name else {
            continue;
        };
        for m in &imp.functions {
            write_extern_method_import(&mut out, target_name, m, module, &imp.target)?;
        }
    }

    // WIT can't represent function overloading, so multiple
    // formalang functions sharing a source name (resolved by argument
    // labels at the call site) collapse to one external name. Track
    // the kebab-cased names already emitted and skip later
    // collisions — the surviving overload is the first declared.
    // Internal call sites still resolve via FunctionId, so the
    // skipped overloads remain reachable from the rest of the module.
    let mut emitted_export_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for &fid in &surface.exports {
        let f = module
            .functions
            .get(fid.0 as usize)
            .ok_or(WitEmitError::ExportOutOfRange {
                index: fid.0,
                len: module.functions.len(),
            })?;
        // formalang's IR doesn't carry a `visibility` field on
        // IrFunction yet, so the survey treats every non-extern
        // top-level function as a candidate export. That over-reports
        // private helper functions (`fn apply(op: (I32) -> I32, ...)`).
        // Skip any function whose signature mentions a non-WIT-
        // expressible type (closures most notably) — those genuinely
        // can't cross the boundary, and treating them as private is
        // the only sound reading. `pub fn` with such a signature is
        // already rejected at preflight.
        if !function_signature_crosses_boundary(f, module) {
            continue;
        }
        let name = kebab_case(&f.name);
        if !emitted_export_names.insert(name) {
            continue;
        }
        write_export(&mut out, f, module)?;
    }

    writeln!(out, "}}").map_err(invalid_format)?;

    parse_round_trip(&out)?;
    Ok(out)
}

fn write_record(
    out: &mut String,
    s: &IrStruct,
    module: &IrModule,
) -> Result<(), WitEmitError> {
    writeln!(out, "  record {} {{", kebab_case(&s.name)).map_err(invalid_format)?;
    for f in &s.fields {
        let wit_ty = resolved_wit_type(&f.ty, module)?.ok_or_else(|| WitEmitError::NeverParam {
            function: format!("record {}", s.name),
            param: f.name.clone(),
        })?;
        writeln!(out, "    {}: {wit_ty},", kebab_case(&f.name)).map_err(invalid_format)?;
    }
    writeln!(out, "  }}").map_err(invalid_format)?;
    Ok(())
}

fn write_variant(
    out: &mut String,
    e: &IrEnum,
    module: &IrModule,
) -> Result<(), WitEmitError> {
    writeln!(out, "  variant {} {{", kebab_case(&e.name)).map_err(invalid_format)?;
    for v in &e.variants {
        let arm_name = kebab_case(&v.name);
        if v.fields.is_empty() {
            writeln!(out, "    {arm_name},").map_err(invalid_format)?;
            continue;
        }
        if v.fields.len() == 1 {
            let f = v.fields.first().ok_or_else(|| WitEmitError::NeverParam {
                function: format!("variant {}", e.name),
                param: "(0-th field)".to_owned(),
            })?;
            let wit_ty = variant_field_wit_type(&e.name, f, module)?;
            writeln!(out, "    {arm_name}({wit_ty}),").map_err(invalid_format)?;
            continue;
        }
        // Multi-field payloads lift as a positional `tuple<T0, T1, ...>`
        // — WIT's variant arm syntax accepts a single payload type, and
        // a tuple is the canonical-ABI representation for a fixed-arity
        // record. Field names don't survive the boundary (WIT tuples
        // are positional); the layout planner already lays the fields
        // out in declaration order so the index→field mapping stays
        // stable across both sides of the boundary.
        let mut tuple = String::from("tuple<");
        for (i, f) in v.fields.iter().enumerate() {
            if i > 0 {
                tuple.push_str(", ");
            }
            let wit_ty = variant_field_wit_type(&e.name, f, module)?;
            tuple.push_str(&wit_ty);
        }
        tuple.push('>');
        writeln!(out, "    {arm_name}({tuple}),").map_err(invalid_format)?;
    }
    writeln!(out, "  }}").map_err(invalid_format)?;
    Ok(())
}

/// Look up the WIT type name for one variant payload field, surfacing
/// a `NeverParam` if the field is `Never`-typed (WIT has no zero-sized
/// element form inside a `tuple<...>` payload).
fn variant_field_wit_type(
    enum_name: &str,
    f: &formalang::ir::IrField,
    module: &IrModule,
) -> Result<String, WitEmitError> {
    resolved_wit_type(&f.ty, module)?.ok_or_else(|| WitEmitError::NeverParam {
        function: format!("variant {enum_name}"),
        param: f.name.clone(),
    })
}

fn write_export(
    out: &mut String,
    f: &IrFunction,
    module: &IrModule,
) -> Result<(), WitEmitError> {
    write_world_func(out, f, "export", module)
}

fn write_import(
    out: &mut String,
    f: &IrFunction,
    module: &IrModule,
) -> Result<(), WitEmitError> {
    write_world_func(out, f, "import", module)
}

/// Emit one WIT `import` line per extern-impl method, using the
/// `<target>-<method>` import name `module_lowering` commits to.
/// The `self` parameter is materialised explicitly: the host sees
/// the struct as its first argument so the import signature
/// matches our declared wasm import. Skip the receiver entirely
/// when the impl target is an empty struct (filtered out of the
/// public surface) — the WIT can't reference a record we won't
/// emit.
fn write_extern_method_import(
    out: &mut String,
    target_name: &str,
    m: &IrFunction,
    module: &IrModule,
    target: &formalang::ir::ImplTarget,
) -> Result<(), WitEmitError> {
    let import_name = format!("{}-{}", kebab_case(target_name), kebab_case(&m.name));

    // Self type: resolve through ImplTarget. Struct targets emit
    // the kebab-cased name iff the struct is non-empty (empty
    // ones are filtered from `surface.exported_structs` and
    // can't appear in a WIT signature).
    let self_ty: Option<String> = match target {
        formalang::ir::ImplTarget::Struct(sid) => {
            let Some(s) = module.structs.get(sid.0 as usize) else {
                return Ok(()); // out-of-range, nothing to emit
            };
            if s.fields.is_empty() {
                None
            } else {
                Some(kebab_case(&s.name))
            }
        }
        formalang::ir::ImplTarget::Enum(eid) => module
            .enums
            .get(eid.0 as usize)
            .map(|e| kebab_case(&e.name)),
        formalang::ir::ImplTarget::Primitive(_) => return Ok(()),
    };

    write!(out, "  import {import_name}: func(").map_err(invalid_format)?;
    let mut wrote_param = false;
    if let Some(ty_name) = &self_ty {
        write!(out, "self: {ty_name}").map_err(invalid_format)?;
        wrote_param = true;
    }
    // Walk explicit params (skip leading `self`).
    for p in m.params.iter().skip(1) {
        if wrote_param {
            out.push_str(", ");
        }
        let ty = p.ty.as_ref().ok_or_else(|| WitEmitError::MissingParamType {
            function: import_name.clone(),
            param: p.name.clone(),
        })?;
        let wit_ty = resolved_wit_type(ty, module)?.ok_or_else(|| WitEmitError::NeverParam {
            function: import_name.clone(),
            param: p.name.clone(),
        })?;
        write!(out, "{}: {wit_ty}", kebab_case(&p.name)).map_err(invalid_format)?;
        wrote_param = true;
    }
    write!(out, ")").map_err(invalid_format)?;
    if let Some(ret) = m.return_type.as_ref()
        && let Some(wit_ty) = resolved_wit_type(ret, module)?
    {
        write!(out, " -> {wit_ty}").map_err(invalid_format)?;
    }
    writeln!(out, ";").map_err(invalid_format)?;
    Ok(())
}

/// Emit a single `import` or `export` line for `f` inside the world
/// block. The signature shape is identical for both directions —
/// only the leading keyword differs.
fn write_world_func(
    out: &mut String,
    f: &IrFunction,
    kind: &str,
    module: &IrModule,
) -> Result<(), WitEmitError> {
    write!(out, "  {kind} {}: func(", kebab_case(&f.name)).map_err(invalid_format)?;
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
        let wit_ty = resolved_wit_type(ty, module)?.ok_or_else(|| WitEmitError::NeverParam {
            function: f.name.clone(),
            param: param.name.clone(),
        })?;
        write!(out, "{}: {wit_ty}", kebab_case(&param.name)).map_err(invalid_format)?;
    }
    write!(out, ")").map_err(invalid_format)?;
    if let Some(ret) = f.return_type.as_ref()
        && let Some(wit_ty) = resolved_wit_type(ret, module)?
    {
        write!(out, " -> {wit_ty}").map_err(invalid_format)?;
    }
    writeln!(out, ";").map_err(invalid_format)?;
    Ok(())
}

/// Map a [`ResolvedType`] to the WIT type name string used inside a
/// `func(...)` signature, or `Ok(None)` for `Never` (no value, omit
/// the result clause when this is a return type — caller's choice).
///
/// `Array<T>` recurses on its element to compose `list<inner>`;
/// `Optional<T>` recurses on its inner to compose `option<inner>`. The
/// element / inner type must itself be WIT-expressible — `Never` and
/// other non-mappable types surface as `NotYetSupported`. The
/// `Optional<Never>` case (the static type of the `nil` literal) is
/// rejected here because WIT has no `option<>`-with-no-payload form;
/// values of that type stay strictly internal.
fn resolved_wit_type(
    ty: &ResolvedType,
    module: &IrModule,
) -> Result<Option<String>, WitEmitError> {
    // Recognise the four prelude compounds — they each desugar
    // through `ResolvedType::Generic { base, args }` and need a
    // structural mapping to a WIT shape (list / option / tuple-of-
    // pairs). Range stays out of the WIT surface; ranges live
    // strictly inside the component.
    match Compound::of(ty, module) {
        Compound::Array(elem) => {
            let inner = resolved_wit_type(elem, module)?.ok_or_else(|| {
                WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: "Array<Never>".to_owned(),
                })
            })?;
            return Ok(Some(format!("list<{inner}>")));
        }
        Compound::Optional(inner) => {
            let inner_name = resolved_wit_type(inner, module)?.ok_or_else(|| {
                WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: "Optional<Never>".to_owned(),
                })
            })?;
            return Ok(Some(format!("option<{inner_name}>")));
        }
        Compound::Dictionary { key, value } => {
            // `Dictionary<K, V>` lifts to `list<tuple<K, V>>` at the
            // boundary — the canonical-ABI shape for a sequence of
            // pairs. Internally we keep the v1 `{ ptr, len, cap }`
            // buffer-of-pair-pointers layout; the list lift reads the
            // header, the per-pair lift reads each `(k, v)` tuple.
            let key_name = resolved_wit_type(key, module)?.ok_or_else(|| {
                WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: "Dictionary<Never, _>".to_owned(),
                })
            })?;
            let value_name = resolved_wit_type(value, module)?.ok_or_else(|| {
                WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: "Dictionary<_, Never>".to_owned(),
                })
            })?;
            return Ok(Some(format!("list<tuple<{key_name}, {value_name}>>")));
        }
        Compound::Range(_) | Compound::None => {}
    }
    match ty {
        ResolvedType::Primitive(p) => primitive_wit_type(*p),
        // `Struct(id)` and `Enum(id)` reference a record / variant
        // declared in the same component's `interface types` block;
        // emitted here as the kebab-cased type name. The struct /
        // enum declaration itself rides the `surface.exported_*`
        // walks above. Looking up via `module.get_struct(id)` keeps
        // the kebab-case logic local.
        ResolvedType::Struct(id) => module.get_struct(*id).map_or_else(
            || {
                Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: format!("Struct(out-of-range #{})", id.0),
                }))
            },
            |s| {
                // Empty pub structs are filtered out of the public
                // surface (wit-component rejects empty records). A
                // function signature that mentions one therefore
                // can't cross the boundary either — surface that as
                // a typed error so the higher-level filter
                // (`function_signature_crosses_boundary`) treats
                // the function as private.
                if s.fields.is_empty() {
                    return Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                        kind: format!(
                            "Struct({}) is empty; wit-component rejects empty records, so the surrounding function cannot cross the WIT boundary",
                            s.name
                        ),
                    }));
                }
                Ok(Some(kebab_case(&s.name)))
            },
        ),
        ResolvedType::Enum(id) => module.get_enum(*id).map_or_else(
            || {
                Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                    kind: format!("Enum(out-of-range #{})", id.0),
                }))
            },
            |e| Ok(Some(kebab_case(&e.name))),
        ),
        ResolvedType::Tuple(fields) => {
            // formalang tuples carry per-position field names but
            // WIT's `tuple<T0, T1, ...>` is positional. Emit the
            // canonical-ABI tuple shape; field names are dropped at
            // the boundary (the layout planner stores them in
            // declaration order so the index→field mapping stays
            // stable on the producer side).
            let mut out = String::from("tuple<");
            for (i, (_name, inner)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let inner_wit =
                    resolved_wit_type(inner, module)?.ok_or_else(|| {
                        WitEmitError::TypeMap(TypeMapError::NotYetSupported {
                            kind: format!("Never inside tuple at position {i}"),
                        })
                    })?;
                out.push_str(&inner_wit);
            }
            out.push('>');
            Ok(Some(out))
        }
        ResolvedType::Trait(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported {
            kind: variant_tag(ty, module),
        })),
    }
}

/// Map a [`PrimitiveType`] to its WIT type name. `Never` returns
/// `None` so callers can decide between rejecting it (parameters) or
/// omitting the result clause (returns).
fn primitive_wit_type(p: PrimitiveType) -> Result<Option<String>, WitEmitError> {
    match p {
        PrimitiveType::I32 => Ok(Some("s32".to_owned())),
        PrimitiveType::I64 => Ok(Some("s64".to_owned())),
        PrimitiveType::F32 => Ok(Some("f32".to_owned())),
        PrimitiveType::F64 => Ok(Some("f64".to_owned())),
        PrimitiveType::Boolean => Ok(Some("bool".to_owned())),
        PrimitiveType::Never => Ok(None),
        // `String` / `Path` / `Regex` all share the canonical-ABI
        // `string` representation at the WIT boundary. Internally they
        // stay distinct (a `Path` value is still a `Path` inside the
        // component), but every public signature exposes them as
        // `string` since WIT has no native `path` / `regex` types.
        PrimitiveType::String | PrimitiveType::Path | PrimitiveType::Regex => {
            Ok(Some("string".to_owned()))
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
/// True iff every type in `f`'s signature can be expressed as a WIT
/// type. Used to skip private helper functions whose signatures
/// reference closure / type-param types that can't cross the
/// boundary; without a visibility field on `IrFunction` we have to
/// infer privacy from signature shape.
fn function_signature_crosses_boundary(f: &IrFunction, module: &IrModule) -> bool {
    for p in &f.params {
        let Some(ty) = p.ty.as_ref() else {
            return false;
        };
        if resolved_wit_type(ty, module).is_err() {
            return false;
        }
    }
    if let Some(ret) = f.return_type.as_ref()
        && resolved_wit_type(ret, module).is_err()
    {
        return false;
    }
    true
}

fn variant_tag(ty: &ResolvedType, module: &IrModule) -> String {
    match Compound::of(ty, module) {
        Compound::Array(_) => return "Array<T>".to_owned(),
        Compound::Range(_) => return "Range<T>".to_owned(),
        Compound::Optional(_) => return "Optional<T>".to_owned(),
        Compound::Dictionary { .. } => return "Dictionary<K, V>".to_owned(),
        Compound::None => {}
    }
    match ty {
        ResolvedType::Primitive(p) => format!("{p:?}"),
        ResolvedType::Struct(_) => "Struct".to_owned(),
        ResolvedType::Trait(_) => "Trait".to_owned(),
        ResolvedType::Enum(_) => "Enum".to_owned(),
        ResolvedType::Tuple(_) => "Tuple".to_owned(),
        ResolvedType::Generic { .. } => "Generic".to_owned(),
        ResolvedType::TypeParam(name) => format!("TypeParam({name})"),
        ResolvedType::External { name, .. } => format!(
            "External({name}) — should have been inlined by upstream MonomorphisePass; reaching the backend means an upstream invariant violation"
        ),
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
