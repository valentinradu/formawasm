//! Tests for `layout::plan_vtable`.
//!
//! Vtables are flat arrays of `i32` funcref-table indices, one slot
//! per trait method. The tests pin down empty / single-method / multi-
//! method cases so virtual-dispatch lowering can rely on a stable
//! `MethodIdx → byte-offset` mapping.

use formalang::ast::{ParamConvention, Visibility};
use formalang::ir::{BindingId, IrFunctionParam, IrFunctionSig, IrModule, IrSpan, IrTrait};
use formawasm::layout::{self, VTABLE_SLOT_ALIGN, VTABLE_SLOT_SIZE, VTableLayout};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn self_param() -> IrFunctionParam {
    IrFunctionParam {
        binding_id: BindingId(0),
        name: "self".to_owned(),
        external_label: None,
        ty: None,
        default: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

fn make_trait(name: &str, method_names: &[&str]) -> IrTrait {
    let methods = method_names
        .iter()
        .map(|m| IrFunctionSig {
            name: (*m).to_owned(),
            params: vec![self_param()],
            return_type: None,
            attributes: Vec::new(),
            span: IrSpan::default(),
        })
        .collect();
    IrTrait {
        name: name.to_owned(),
        visibility: Visibility::Public,
        composed_traits: Vec::new(),
        fields: Vec::new(),
        methods,
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn assert_vtable(actual: &VTableLayout, method_count: u32) -> TestResult {
    let expected = VTableLayout {
        size: method_count.saturating_mul(VTABLE_SLOT_SIZE),
        align: VTABLE_SLOT_ALIGN,
        method_count,
        slot_size: VTABLE_SLOT_SIZE,
    };
    if *actual != expected {
        return Err(format!("layout: got {actual:?}, want {expected:?}").into());
    }
    Ok(())
}

#[test]
fn empty_trait_yields_zero_size_vtable() -> TestResult {
    let module = IrModule::new();
    let t = make_trait("Marker", &[]);
    let layout = layout::plan_vtable(&t, &module)?;
    assert_vtable(&layout, 0)
}

#[test]
fn single_method_trait_yields_one_slot() -> TestResult {
    let module = IrModule::new();
    let t = make_trait("Show", &["show"]);
    let layout = layout::plan_vtable(&t, &module)?;
    assert_vtable(&layout, 1)
}

#[test]
fn multi_method_trait_packs_one_slot_per_method() -> TestResult {
    let module = IrModule::new();
    let t = make_trait("Iterator", &["next", "peek", "reset"]);
    let layout = layout::plan_vtable(&t, &module)?;
    assert_vtable(&layout, 3)
}

#[test]
fn slot_constants_match_funcref_index_size() -> TestResult {
    // Vtable slots store funcref-table indices as i32s — pin the
    // values so the lowering pass that materialises the table can
    // rely on a 4-byte stride without re-deriving it.
    if VTABLE_SLOT_SIZE != 4 {
        return Err(format!("VTABLE_SLOT_SIZE: got {VTABLE_SLOT_SIZE}, want 4").into());
    }
    if VTABLE_SLOT_ALIGN != 4 {
        return Err(format!("VTABLE_SLOT_ALIGN: got {VTABLE_SLOT_ALIGN}, want 4").into());
    }
    Ok(())
}
