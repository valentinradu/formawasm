//! Tests for `layout::plan_string`.
//!
//! `plan_string` returns the same `{ ptr, len }` header for every
//! string value — there's nothing that varies by inner type the way
//! `plan_array` / `plan_optional` do. The tests pin down the byte
//! offsets and sizes so any future layout change has to come with an
//! intentional update here.

use formalang::ir::IrModule;
use formawasm::layout::{
    self, STRING_HEADER_ALIGN, STRING_HEADER_SIZE, STRING_LEN_OFFSET, STRING_PTR_OFFSET,
    StringLayout,
};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[test]
fn string_header_is_eight_bytes_with_two_i32_slots() -> TestResult {
    let module = IrModule::new();
    let actual = layout::plan_string(&module);
    let expected = StringLayout {
        header_size: STRING_HEADER_SIZE,
        header_align: STRING_HEADER_ALIGN,
        ptr_offset: STRING_PTR_OFFSET,
        len_offset: STRING_LEN_OFFSET,
    };
    if actual != expected {
        return Err(format!("layout: got {actual:?}, want {expected:?}").into());
    }
    Ok(())
}

#[test]
fn string_header_constants_match_canonical_abi() -> TestResult {
    // The component-model canonical ABI lifts a `string` return as
    // `(ptr: i32, len: i32)` at offsets 0 / 4 — same shape we emit
    // internally. Pin the exact values so a regression here would
    // surface as a layout-test failure rather than a silent boundary
    // mismatch when the WIT-mapping mc lands.
    if STRING_HEADER_SIZE != 8 {
        return Err(format!("STRING_HEADER_SIZE: got {STRING_HEADER_SIZE}, want 8").into());
    }
    if STRING_HEADER_ALIGN != 4 {
        return Err(format!("STRING_HEADER_ALIGN: got {STRING_HEADER_ALIGN}, want 4").into());
    }
    if STRING_PTR_OFFSET != 0 {
        return Err(format!("STRING_PTR_OFFSET: got {STRING_PTR_OFFSET}, want 0").into());
    }
    if STRING_LEN_OFFSET != 4 {
        return Err(format!("STRING_LEN_OFFSET: got {STRING_LEN_OFFSET}, want 4").into());
    }
    Ok(())
}
