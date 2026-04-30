//! Builder for the core WebAssembly module the backend assembles
//! during code generation.
//!
//! [`ModuleBuilder`] starts from an empty validating module that
//! already declares one linear memory and one mutable `i32` heap
//! pointer global. Subsequent phases plug additional sections (types,
//! functions, code, exports …) into the builder before
//! [`ModuleBuilder::finish`] emits the byte-encoded module.

use std::borrow::Cow;

use wasm_encoder::{
    CodeSection, ConstExpr, ElementSection, Elements, ExportKind, ExportSection, Function,
    FunctionSection, GlobalSection, GlobalType, MemorySection, MemoryType, Module, RefType,
    TableSection, TableType, TypeSection, ValType,
};

/// Alignment, in bytes, the bump allocator rounds every returned address up to.
///
/// Picked to satisfy the largest primitive alignment the canonical
/// ABI imposes (`i64` / `f64` = 8); over-aligning the smaller
/// primitives wastes a handful of bytes per allocation, which is
/// acceptable for Phase 1b where allocations are infrequent.
pub const BUMP_ALLOCATOR_ALIGN: u32 = 8;

/// Source-level name we assign to the bump-allocator helper. Not
/// exported, but kept stable so debug tooling can identify it.
pub const BUMP_ALLOCATOR_NAME: &str = "__alloc";

/// Export name under which the runtime memory is published. Required
/// by `wit-component`'s canonical-ABI lifting/lowering and useful for
/// tests that need to peek at constructed aggregates.
pub const MEMORY_EXPORT_NAME: &str = "memory";

/// Round-up addend for the bump allocator's alignment math:
/// `BUMP_ALLOCATOR_ALIGN - 1`. Hardcoded as `i32` to feed
/// `wasm_encoder::Instruction::i32_const` without going through a
/// fallible `u32 → i32` cast at call time.
const BUMP_ALIGN_ROUND_UP_ADDEND: i32 = 7;

/// Bitmask the bump allocator AND-s with to clear the low alignment
/// bits: `!(BUMP_ALLOCATOR_ALIGN - 1)` reinterpreted as `i32`. Same
/// reason as above for the hand-coded value.
const BUMP_ALIGN_KEEP_MASK: i32 = -8;

const _: () = {
    assert!(
        BUMP_ALLOCATOR_ALIGN == 8,
        "BUMP_ALIGN_ROUND_UP_ADDEND / BUMP_ALIGN_KEEP_MASK assume 8-byte alignment",
    );
};

/// Index of the single linear memory the runtime uses for all heap
/// allocations.
pub const MEMORY_INDEX: u32 = 0;

/// Index of the heap-pointer global. Bumped by the bump allocator
/// emitted later in Phase 1b.
pub const HEAP_PTR_GLOBAL_INDEX: u32 = 0;

/// Initial heap pointer, in bytes from the start of linear memory.
/// Reserved bytes below this address are available for future
/// runtime helpers (jump tables, vtables, string literals, …).
pub const HEAP_BASE: i32 = 0;

/// Initial size of the linear memory, in 64-`KiB` Wasm pages.
pub const INITIAL_MEMORY_PAGES: u64 = 1;

/// In-progress core-Wasm module. Currently carries the runtime memory
/// and heap-pointer global; later phases plug type, function, code,
/// and export sections into the same builder.
#[derive(Debug)]
#[non_exhaustive]
pub struct ModuleBuilder {
    types: TypeSection,
    functions: FunctionSection,
    tables: TableSection,
    memories: MemorySection,
    globals: GlobalSection,
    exports: ExportSection,
    elements: ElementSection,
    code: CodeSection,
    bump_allocator: Option<u32>,
    /// Index of the funcref `Table` carrying every closure-callable
    /// function. Created lazily by [`Self::declare_closure_table`]; the
    /// `ElementSection` populates it with concrete `wasm` function
    /// indices via [`Self::populate_closure_table`].
    closure_table_idx: Option<u32>,
}

impl Default for ModuleBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleBuilder {
    /// Build an empty validating module with the runtime memory +
    /// heap-pointer global already in place.
    #[must_use]
    pub fn new() -> Self {
        let mut memories = MemorySection::new();
        memories.memory(MemoryType {
            minimum: INITIAL_MEMORY_PAGES,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });

        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(HEAP_BASE),
        );

        let mut exports = ExportSection::new();
        exports.export(MEMORY_EXPORT_NAME, ExportKind::Memory, MEMORY_INDEX);

        Self {
            types: TypeSection::new(),
            functions: FunctionSection::new(),
            tables: TableSection::new(),
            memories,
            globals,
            exports,
            elements: ElementSection::new(),
            code: CodeSection::new(),
            bump_allocator: None,
            closure_table_idx: None,
        }
    }

    /// Declare a wasm function-type signature without attaching a
    /// function body. Used by `call_indirect` lowerings that need a
    /// type-index for the call's signature; the index is stable across
    /// the rest of the build.
    pub fn declare_type(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        let idx = self.types.len();
        self.types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        idx
    }

    /// Lazy-create the funcref `Table` of `num_closures` slots used by
    /// indirect closure invocation. Returns the table index. Calling
    /// repeatedly returns the original index without re-declaring.
    pub fn declare_closure_table(&mut self, num_closures: u32) -> u32 {
        if let Some(idx) = self.closure_table_idx {
            return idx;
        }
        let idx = self.tables.len();
        self.tables.table(TableType {
            element_type: RefType::FUNCREF,
            minimum: u64::from(num_closures),
            maximum: Some(u64::from(num_closures)),
            table64: false,
            shared: false,
        });
        self.closure_table_idx = Some(idx);
        idx
    }

    /// Populate the closure funcref table with `wasm_func_indices`,
    /// in order, starting at element offset 0. The closure value's
    /// stored funcref index is the slot number (the table's element
    /// index), which `call_indirect` resolves to the function pointer
    /// at runtime.
    pub fn populate_closure_table(&mut self, wasm_func_indices: &[u32]) {
        let Some(table_idx) = self.closure_table_idx else {
            return;
        };
        self.elements.active(
            Some(table_idx),
            &ConstExpr::i32_const(0),
            Elements::Functions(Cow::Borrowed(wasm_func_indices)),
        );
    }

    /// Wasm table index of the closure funcref table if declared.
    #[must_use]
    pub const fn closure_table_index(&self) -> Option<u32> {
        self.closure_table_idx
    }

    /// Export a previously-declared function under `name`.
    pub fn export_function(&mut self, name: &str, function_index: u32) {
        self.exports.export(name, ExportKind::Func, function_index);
    }

    /// Declare a function with the given param + result valtypes and a
    /// caller-supplied body. The body must already include the closing
    /// `end` instruction. Returns the wasm function index.
    pub fn declare_function_with_body(
        &mut self,
        param_types: &[ValType],
        result_types: &[ValType],
        body: &Function,
    ) -> u32 {
        let type_index = self.types.len();
        let func_index = self.functions.len();

        self.types
            .ty()
            .function(param_types.iter().copied(), result_types.iter().copied());
        self.functions.function(type_index);
        self.code.function(body);

        func_index
    }

    /// Declare a function with the given param + result valtypes and a
    /// placeholder `unreachable` body. Useful for tests and for laying
    /// down the function-index space before bodies are lowered.
    pub fn declare_function(&mut self, param_types: &[ValType], result_types: &[ValType]) -> u32 {
        let mut body = Function::new(core::iter::empty());
        body.instructions().unreachable().end();
        self.declare_function_with_body(param_types, result_types, &body)
    }

    /// Declare and emit the bump-allocator runtime helper, returning
    /// its wasm function index. Subsequent calls return the same
    /// index without re-emitting the function — the helper is meant
    /// to live exactly once per module.
    ///
    /// Signature: `__alloc(size: i32) -> i32` (raw byte size in,
    /// allocation address out). Each returned address is aligned up
    /// to [`BUMP_ALLOCATOR_ALIGN`] bytes; the heap pointer global is
    /// advanced past the allocation. Out-of-memory is not yet handled
    /// — callers that bust the linear-memory ceiling get a wasm trap.
    pub fn declare_bump_allocator(&mut self) -> u32 {
        if let Some(idx) = self.bump_allocator {
            return idx;
        }

        let mut body = Function::new(core::iter::once((1, ValType::I32)));
        let mut i = body.instructions();
        // local 0 = size param, local 1 = aligned base pointer.
        // aligned_ptr = (heap_ptr + (ALIGN - 1)) & ~(ALIGN - 1)
        i.global_get(HEAP_PTR_GLOBAL_INDEX)
            .i32_const(BUMP_ALIGN_ROUND_UP_ADDEND)
            .i32_add()
            .i32_const(BUMP_ALIGN_KEEP_MASK)
            .i32_and()
            .local_tee(1)
            // new heap_ptr = aligned_ptr + size
            .local_get(0)
            .i32_add()
            .global_set(HEAP_PTR_GLOBAL_INDEX)
            // return aligned_ptr
            .local_get(1)
            .end();

        let idx = self.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
        self.bump_allocator = Some(idx);
        idx
    }

    /// Wasm function index of the bump allocator if it has been
    /// declared, else `None`.
    #[must_use]
    pub const fn bump_allocator_index(&self) -> Option<u32> {
        self.bump_allocator
    }

    /// Number of declared functions so far.
    #[must_use]
    pub fn function_count(&self) -> u32 {
        self.functions.len()
    }

    /// Encode the module as a sequence of bytes ready for validation
    /// or component wrapping.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let mut module = Module::new();
        // Section order is fixed by the wasm spec: type, function,
        // table, memory, global, export, element, code.
        if !self.types.is_empty() {
            module.section(&self.types);
        }
        if !self.functions.is_empty() {
            module.section(&self.functions);
        }
        if !self.tables.is_empty() {
            module.section(&self.tables);
        }
        module.section(&self.memories);
        module.section(&self.globals);
        if !self.exports.is_empty() {
            module.section(&self.exports);
        }
        if !self.elements.is_empty() {
            module.section(&self.elements);
        }
        if !self.code.is_empty() {
            module.section(&self.code);
        }
        module.finish()
    }
}
