//! Tests for `Literal::Nil` lowering.
//!
//! Build a small module whose entry point returns a tag-only
//! `Optional<Never>` value built from `nil`, lower through the
//! production `module_lowering::lower_module` pipeline, validate,
//! instantiate under wasmtime, and read back the discriminant tag
//! through the exported memory.

mod common;
use common::{optional_ty, seed_prelude};

use formalang::ast::{Literal, PrimitiveType};
use formalang::ir::{
    BindingId, IrBlockStatement, IrExpr, IrFunction, IrModule, IrSpan, ResolvedType,
};
use formawasm::layout::{OPTIONAL_TAG_NIL, OPTIONAL_TAG_SIZE};
use formawasm::module_lowering;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::{Engine, Instance, Module, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn optional(inner: ResolvedType) -> ResolvedType {
    optional_ty(inner)
}

fn nil_literal() -> IrExpr {
    IrExpr::Literal {
        value: Literal::Nil,
        ty: optional(primitive(PrimitiveType::Never)),
        span: IrSpan::default(),
    }
}

fn function(name: &str, return_ty: ResolvedType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(return_ty),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn instantiate(bytes: &[u8]) -> Result<(Store<()>, Instance), TestError> {
    let engine = Engine::default();
    let m = Module::from_binary(&engine, bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    Ok((store, instance))
}

fn read_tag(store: &mut Store<()>, instance: &Instance, ptr: i32) -> Result<u32, TestError> {
    let memory = instance
        .exports(&mut *store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;
    let off = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    let mut buf = [0u8; 4];
    memory.read(&*store, off, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

#[test]
fn nil_literal_returns_pointer_to_zero_tag() -> TestResult {
    // `fn make() -> Never? { nil }` — the simplest end-to-end exercise
    // for the new Literal::Nil lowering path.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "make",
        optional(primitive(PrimitiveType::Never)),
        nil_literal(),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;
    let tag = read_tag(&mut store, &instance, ptr)?;
    if tag != OPTIONAL_TAG_NIL {
        return Err(format!("tag at returned pointer: got {tag}, want {OPTIONAL_TAG_NIL}").into());
    }
    Ok(())
}

#[test]
fn nil_widens_into_optional_i32_let_binding() -> TestResult {
    // `fn make() -> Never? {
    //     let x: I32? = nil
    //     x
    // }`
    //
    // `x` is typed `Optional<I32>` but assigned `nil` whose static
    // type is `Optional<Never>`. Both lower to an i32 pointer in the
    // function body, so the let binding accepts the storage-
    // compatible pointer without a wrap. The function returns the
    // same pointer (typed back to `Optional<Never>` via the body
    // expression's static type), which lets the test read the tag
    // off the same allocation we created at let-binding time.
    let binding_id = BindingId(0);
    let body = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id,
            name: "x".to_owned(),
            mutable: false,
            ty: Some(optional(primitive(PrimitiveType::I32))),
            value: nil_literal(),
            span: IrSpan::default(),
        }],
        result: Box::new(IrExpr::LetRef {
            binding_id,
            name: "x".to_owned(),
            ty: optional(primitive(PrimitiveType::Never)),
            span: IrSpan::default(),
        }),
        ty: optional(primitive(PrimitiveType::Never)),
        span: IrSpan::default(),
    };

    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "make",
        optional(primitive(PrimitiveType::Never)),
        body,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    // The returned pointer references an OPTIONAL_TAG_SIZE-byte
    // allocation whose first 4 bytes carry `OPTIONAL_TAG_NIL`. The
    // bump allocator hands out addresses starting at HEAP_BASE = 0,
    // so a zero pointer is the legitimate first allocation here.
    let tag = read_tag(&mut store, &instance, ptr)?;
    if tag != OPTIONAL_TAG_NIL {
        return Err(format!("tag at returned pointer: got {tag}, want {OPTIONAL_TAG_NIL}").into());
    }
    let _ = OPTIONAL_TAG_SIZE; // documents the allocation size
    Ok(())
}
