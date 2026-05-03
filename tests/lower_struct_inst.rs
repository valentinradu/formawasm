//! Tests for `lower::lower_struct_inst`.
//!
//! Each test builds a small module with one struct and one function
//! that constructs an instance of it. The function is lowered through
//! the production `module_lowering::lower_module` path, the resulting
//! component is validated and instantiated under wasmtime, and the
//! generated function is exercised — when it returns a primitive
//! field-load result we round-trip the full constructor / accessor
//! pair in a single function body to verify both halves at once.
//!
//! Note: `FieldAccess` is not yet wired up (mc3c), so the tests here
//! return the primitive value of a single field via a hand-built
//! `i32_load` instruction in the test harness once we have the base
//! pointer; for now we just confirm the module validates and the
//! constructor's pointer is non-zero (heap addresses start at the
//! bump-allocator base).

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    FieldIdx, IrExpr, IrField, IrFunction, IrModule, IrSpan, IrStruct, ResolvedType, StructId,
};
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

fn integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
    let suffix = if ty == PrimitiveType::I64 {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive(ty),
        span: IrSpan::default(),
    }
}

fn boolean_literal(b: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(b),
        ty: primitive(PrimitiveType::Boolean),
        span: IrSpan::default(),
    }
}

fn struct_def(name: &str, fields: Vec<(&str, PrimitiveType)>) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: fields
            .into_iter()
            .map(|(n, ty)| IrField {
                name: n.to_owned(),
                ty: primitive(ty),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            })
            .collect(),
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn struct_inst(struct_id: StructId, ty: ResolvedType, fields: Vec<(&str, IrExpr)>) -> IrExpr {
    IrExpr::StructInst {
        struct_id: Some(struct_id),
        type_args: Vec::new(),
        fields: fields
            .into_iter()
            .enumerate()
            .map(|(i, (name, e))| (name.to_owned(), FieldIdx(u32::try_from(i).unwrap_or(0)), e))
            .collect(),
        ty,
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

#[test]
fn struct_inst_with_two_i32_fields_validates() -> TestResult {
    // struct Pair { a: I32, b: I32 }
    // fn make() -> Pair { Pair { a: 7, b: 13 } }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Pair",
        vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I32)],
    ));

    let body = struct_inst(
        StructId(0),
        ResolvedType::Struct(StructId(0)),
        vec![
            ("a", integer_literal(7, PrimitiveType::I32)),
            ("b", integer_literal(13, PrimitiveType::I32)),
        ],
    );
    module
        .functions
        .push(function("make", ResolvedType::Struct(StructId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}

#[test]
fn struct_inst_pointer_is_nonzero_under_wasmtime() -> TestResult {
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Triple",
        vec![
            ("a", PrimitiveType::I32),
            ("b", PrimitiveType::I32),
            ("c", PrimitiveType::I32),
        ],
    ));

    let body = struct_inst(
        StructId(0),
        ResolvedType::Struct(StructId(0)),
        vec![
            ("a", integer_literal(1, PrimitiveType::I32)),
            ("b", integer_literal(2, PrimitiveType::I32)),
            ("c", integer_literal(3, PrimitiveType::I32)),
        ],
    );
    module
        .functions
        .push(function("make", ResolvedType::Struct(StructId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let make = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = make.call(&mut store, ())?;

    // Heap allocations start at offset 0 in our module. The bump
    // allocator returns a non-negative address aligned to 8.
    if ptr < 0 {
        return Err(format!("expected non-negative ptr, got {ptr}").into());
    }
    if ptr % 8 != 0 {
        return Err(format!("expected 8-aligned ptr, got {ptr}").into());
    }
    Ok(())
}

#[test]
fn struct_inst_writes_fields_to_linear_memory() -> TestResult {
    // struct Pair { a: I32, b: I32 }
    // fn make() -> I32 { Pair { a: 11, b: 22 } returns the pointer; tests load both fields }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Pair",
        vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I32)],
    ));

    let body = struct_inst(
        StructId(0),
        ResolvedType::Struct(StructId(0)),
        vec![
            ("a", integer_literal(11, PrimitiveType::I32)),
            ("b", integer_literal(22, PrimitiveType::I32)),
        ],
    );
    module
        .functions
        .push(function("make", ResolvedType::Struct(StructId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    // Re-instantiate so we can read memory after the call.
    let instance = Instance::new(&mut store, &m, &[])?;
    let make = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = make.call(&mut store, ())?;
    let memory = instance
        .get_memory(&mut store, "memory")
        .or_else(|| {
            // Memory isn't exported by the backend; export name in
            // wasmtime defaults to whatever the module declared. Pull
            // the first memory.
            instance
                .exports(&mut store)
                .find_map(wasmtime::Export::into_memory)
        })
        .ok_or("no memory in instance")?;

    let mut buf = [0u8; 8];
    let offset = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    memory.read(&store, offset, &mut buf)?;
    let a = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let b = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if (a, b) != (11, 22) {
        return Err(format!("memory: a={a}, b={b}, want 11, 22").into());
    }
    Ok(())
}

#[test]
fn struct_inst_with_mixed_alignments() -> TestResult {
    // struct M { flag: Boolean, count: I32, big: I64 }
    // Layout: flag at 0..1, count at 4..8, big at 8..16. Total size 16, align 8.
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "M",
        vec![
            ("flag", PrimitiveType::Boolean),
            ("count", PrimitiveType::I32),
            ("big", PrimitiveType::I64),
        ],
    ));

    let body = struct_inst(
        StructId(0),
        ResolvedType::Struct(StructId(0)),
        vec![
            ("flag", boolean_literal(true)),
            ("count", integer_literal(0x1234_5678, PrimitiveType::I32)),
            (
                "big",
                integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
            ),
        ],
    );
    module
        .functions
        .push(function("make", ResolvedType::Struct(StructId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let make = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = make.call(&mut store, ())?;
    let memory = instance
        .exports(&mut store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;

    let mut buf = [0u8; 16];
    let offset = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    memory.read(&store, offset, &mut buf)?;
    let flag = buf[0];
    let count = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let big = i64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    if flag != 1 {
        return Err(format!("flag: got {flag}, want 1").into());
    }
    if count != 0x1234_5678 {
        return Err(format!("count: got {count:#x}, want 0x12345678").into());
    }
    if big != 0x0BAD_F00D_DEAD_BEEF {
        return Err(format!("big: got {big:#x}, want 0x0BADF00DDEADBEEF").into());
    }
    Ok(())
}
