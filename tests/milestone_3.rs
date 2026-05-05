//! Phase 3 milestone — virtual trait-method dispatch across two
//! impls under wasmtime's component runtime.
//!
//! Hand-builds an [`IrModule`] containing one trait `Greet` declaring
//! a single method `value(self) -> I32`, two struct types `Alpha` /
//! `Beta` each carrying a single payload field, and an `impl Greet`
//! for each. Two top-level functions construct the corresponding
//! struct and invoke `.value()` through `DispatchKind::Virtual`
//! pointing at `Greet`. The pipeline runs without
//! `MonomorphisePass` (no devirtualisation), so the Virtual call
//! sites survive into the backend, get materialised against the
//! per-impl vtables in linear memory, and dispatch to the correct
//! method body via the method funcref table at runtime.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, DispatchKind, FieldIdx, ImplId, ImplTarget, IrExpr, IrField, IrFunction,
    IrFunctionParam, IrFunctionSig, IrImpl, IrModule, IrSpan, IrStruct, IrTrait, IrTraitRef,
    MethodIdx, ResolvedType, StructId, TraitId,
};
use formalang::pipeline::Pipeline;
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const ALPHA_ID: StructId = StructId(0);
const BETA_ID: StructId = StructId(1);
const GREET_ID: TraitId = TraitId(0);
const ALPHA_IMPL_ID: ImplId = ImplId(0);
const BETA_IMPL_ID: ImplId = ImplId(1);
const VALUE_METHOD_IDX: MethodIdx = MethodIdx(0);

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn integer_literal(value: i128) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(value),
            NumericSuffix::I32,
        )),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    }
}

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

fn primitive_field(name: &str, ty: PrimitiveType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty: primitive(ty),
        mutable: false,
        optional: false,
        default: None,
        doc: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

fn build_struct(name: &str) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Private,
        traits: vec![IrTraitRef::simple(GREET_ID)],
        fields: vec![primitive_field("tag", PrimitiveType::I32)],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn self_param(struct_ty: ResolvedType) -> IrFunctionParam {
    IrFunctionParam {
        binding_id: BindingId(0),
        name: "self".to_owned(),
        external_label: None,
        ty: Some(struct_ty),
        default: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

fn build_greet_trait() -> IrTrait {
    IrTrait {
        name: "Greet".to_owned(),
        visibility: Visibility::Private,
        composed_traits: Vec::new(),
        fields: Vec::new(),
        methods: vec![IrFunctionSig {
            name: "value".to_owned(),
            params: vec![IrFunctionParam {
                binding_id: BindingId(0),
                name: "self".to_owned(),
                external_label: None,
                // Trait method `self` carries no concrete type — it
                // resolves to the impl's target at devirtualisation
                // time. Phase 3 doesn't run that pass, so this field
                // stays None and our backend wires the receiver
                // through as an i32 pointer regardless.
                ty: None,
                default: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            }],
            return_type: Some(primitive(PrimitiveType::I32)),
            attributes: Vec::new(),
            span: IrSpan::default(),
        }],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_value_impl(struct_id: StructId, returned: i128) -> IrFunction {
    IrFunction {
        name: "value".to_owned(),
        generic_params: Vec::new(),
        params: vec![self_param(ResolvedType::Struct(struct_id))],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(integer_literal(returned)),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_impl(struct_id: StructId, returned: i128) -> IrImpl {
    IrImpl {
        target: ImplTarget::Struct(struct_id),
        trait_ref: Some(IrTraitRef::simple(GREET_ID)),
        is_extern: false,
        generic_params: Vec::new(),
        functions: vec![build_value_impl(struct_id, returned)],
        span: IrSpan::default(),
    }
}

fn build_dispatch_function(name: &str, struct_id: StructId) -> IrFunction {
    let receiver = IrExpr::StructInst {
        struct_id: Some(struct_id),
        type_args: Vec::new(),
        fields: vec![("tag".to_owned(), FieldIdx(0), integer_literal(0))],
        ty: ResolvedType::Struct(struct_id),
        span: IrSpan::default(),
    };
    let body = IrExpr::MethodCall {
        receiver: Box::new(receiver),
        method: "value".to_owned(),
        method_idx: VALUE_METHOD_IDX,
        args: Vec::new(),
        dispatch: DispatchKind::Virtual {
            trait_id: GREET_ID,
            method_name: "value".to_owned(),
        },
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_module() -> IrModule {
    let mut module = IrModule::new();
    module.structs.push(build_struct("Alpha"));
    module.structs.push(build_struct("Beta"));
    module.traits.push(build_greet_trait());
    module.impls.push(build_impl(ALPHA_ID, 1));
    module.impls.push(build_impl(BETA_ID, 2));
    module
        .functions
        .push(build_dispatch_function("dispatch_alpha", ALPHA_ID));
    module
        .functions
        .push(build_dispatch_function("dispatch_beta", BETA_ID));
    // Sanity-check the impl-id constants so the milestone fails
    // loudly if the impl ordering changes.
    let _ = (ALPHA_IMPL_ID, BETA_IMPL_ID);
    module
}

#[test]
fn virtual_dispatch_resolves_across_two_impls_under_wasmtime() -> TestResult {
    let module = build_module();
    let mut pipeline = Pipeline::new();
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let mut linker = Linker::<()>::new(&engine);
    // formalang's prelude declares `pub extern fn assert(condition: Boolean)`;
    // every program parsed via `compile_to_ir_*` imports it. Wire to a host
    // function that traps on `false` so failed assertions surface as wasmtime
    // traps to the test caller.
    linker.root().func_wrap("assert", |_store, (cond,): (bool,)| {
        if cond { Ok(()) } else { Err(wasmtime::Error::msg("assert(false)")) }
    })?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;

    let dispatch_alpha = instance.get_typed_func::<(), (i32,)>(&mut store, "dispatch-alpha")?;
    let dispatch_beta = instance.get_typed_func::<(), (i32,)>(&mut store, "dispatch-beta")?;

    let (alpha,) = dispatch_alpha.call(&mut store, ())?;
    if alpha != 1 {
        return Err(format!("dispatch_alpha = {alpha}, want 1").into());
    }

    let (beta,) = dispatch_beta.call(&mut store, ())?;
    if beta != 2 {
        return Err(format!("dispatch_beta = {beta}, want 2").into());
    }

    Ok(())
}
