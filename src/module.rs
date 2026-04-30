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
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, Elements, ExportKind,
    ExportSection, Function, FunctionSection, GlobalSection, GlobalType, MemArg, MemorySection,
    MemoryType, Module, RefType, TableSection, TableType, TypeSection, ValType,
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

/// Source-level name for the byte-by-byte string-equality helper.
/// Returns 1 if both inputs (each an `{ ptr, len }` header pointer)
/// describe identical byte sequences, 0 otherwise.
pub const STR_EQ_NAME: &str = "__str_eq";

/// Source-level name for the string-concatenation helper. Allocates a
/// fresh byte buffer plus an 8-byte header and copies both inputs
/// (each an `{ ptr, len }` header pointer) into the new buffer.
pub const STR_CONCAT_NAME: &str = "__str_concat";

/// Canonical-ABI export name the host calls when it needs to allocate
/// (or grow) a buffer in our linear memory before passing a `string`
/// or `list<T>` argument across the component boundary.
pub const CABI_REALLOC_NAME: &str = "cabi_realloc";

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

/// In-progress core-Wasm module.
///
/// Carries the runtime memory plus every wasm section the backend
/// assembles; the heap-pointer global and the data section are
/// emitted at [`Self::finish`] time so the initial heap-pointer value
/// can be relocated past whatever string-literal data has been seeded
/// in the meantime.
#[derive(Debug)]
#[non_exhaustive]
pub struct ModuleBuilder {
    types: TypeSection,
    functions: FunctionSection,
    tables: TableSection,
    memories: MemorySection,
    exports: ExportSection,
    elements: ElementSection,
    code: CodeSection,
    bump_allocator: Option<u32>,
    /// Index of the byte-by-byte string-equality helper. Lazily
    /// declared by [`Self::declare_str_eq`].
    str_eq: Option<u32>,
    /// Index of the string-concatenation helper. Lazily declared by
    /// [`Self::declare_str_concat`].
    str_concat: Option<u32>,
    /// Index of the funcref `Table` carrying every closure-callable
    /// function. Created lazily by [`Self::declare_closure_table`]; the
    /// `ElementSection` populates it with concrete `wasm` function
    /// indices via [`Self::populate_closure_table`].
    closure_table_idx: Option<u32>,
    /// Static data segment seeded with string-literal bytes + headers.
    /// Lives at offset 0 in linear memory; the bump-allocator's heap
    /// starts immediately after this region.
    string_data: Vec<u8>,
}

impl Default for ModuleBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleBuilder {
    /// Build an empty validating module with the runtime memory in
    /// place. The heap-pointer global is materialized at
    /// [`Self::finish`] time so its initial value can reflect any
    /// static data segments accumulated meanwhile.
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

        let mut exports = ExportSection::new();
        exports.export(MEMORY_EXPORT_NAME, ExportKind::Memory, MEMORY_INDEX);

        Self {
            types: TypeSection::new(),
            functions: FunctionSection::new(),
            tables: TableSection::new(),
            memories,
            exports,
            elements: ElementSection::new(),
            code: CodeSection::new(),
            bump_allocator: None,
            str_eq: None,
            str_concat: None,
            closure_table_idx: None,
            string_data: Vec::new(),
        }
    }

    /// Install `bytes` as the contents of the static data segment.
    ///
    /// The segment is emitted as an active data segment at offset 0 of
    /// linear memory at [`Self::finish`] time; the heap-pointer
    /// global's initial value is bumped past the end of the segment
    /// (rounded up to [`BUMP_ALLOCATOR_ALIGN`]) so subsequent bump-
    /// allocator calls cannot trample the data. Calling repeatedly
    /// replaces the previously-installed contents.
    pub fn set_string_data(&mut self, bytes: Vec<u8>) {
        self.string_data = bytes;
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

    /// Declare and emit the `__str_eq` runtime helper, returning its
    /// wasm function index. Subsequent calls return the same index.
    ///
    /// Signature: `__str_eq(a: i32, b: i32) -> i32`. Each input is a
    /// pointer to an `{ ptr, len }` string header. Returns 1 when the
    /// two strings have identical byte sequences, 0 otherwise.
    ///
    /// Implementation: load both `len` slots; if they differ, return
    /// 0. Otherwise loop over the bytes pointed at by each `ptr`,
    /// comparing one byte at a time. Returns 1 once the loop runs to
    /// completion without finding a mismatch.
    pub fn declare_str_eq(&mut self) -> u32 {
        if let Some(idx) = self.str_eq {
            return idx;
        }

        // Locals: 2 i32 — local 2 = len, local 3 = i (loop counter).
        // Params live at locals 0 / 1 (a, b).
        let mut body = Function::new(core::iter::once((2, ValType::I32)));
        let mut i = body.instructions();

        // local.get a; i32.load offset=4   ;; a.len
        // local.get b; i32.load offset=4   ;; b.len
        // i32.ne                            ;; lengths differ?
        // if -> i32.const 0; return ; end
        i.local_get(0)
            .i32_load(MemArg {
                offset: u64::from(crate::layout::STRING_LEN_OFFSET),
                align: 2, // log2(4)
                memory_index: MEMORY_INDEX,
            })
            .local_tee(2) // stash a.len in local 2
            .local_get(1)
            .i32_load(MemArg {
                offset: u64::from(crate::layout::STRING_LEN_OFFSET),
                align: 2,
                memory_index: MEMORY_INDEX,
            })
            .i32_ne()
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();

        // local.set i = 0
        i.i32_const(0).local_set(3);

        // Replace param locals 0, 1 with their `ptr` slots so the
        // byte loop indexes off ptr + i directly.
        i.local_get(0)
            .i32_load(MemArg {
                offset: u64::from(crate::layout::STRING_PTR_OFFSET),
                align: 2,
                memory_index: MEMORY_INDEX,
            })
            .local_set(0);
        i.local_get(1)
            .i32_load(MemArg {
                offset: u64::from(crate::layout::STRING_PTR_OFFSET),
                align: 2,
                memory_index: MEMORY_INDEX,
            })
            .local_set(1);

        // block $done
        //   loop $cmp
        //     local.get i; local.get len; i32.ge_u; br_if $done
        //     local.get a_ptr; local.get i; i32.add; i32.load8_u
        //     local.get b_ptr; local.get i; i32.add; i32.load8_u
        //     i32.ne
        //     if -> i32.const 0; return ; end
        //     local.get i; i32.const 1; i32.add; local.set i
        //     br $cmp
        //   end
        // end
        i.block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(3)
            .local_get(2)
            .i32_ge_u()
            .br_if(1)
            // a_ptr + i
            .local_get(0)
            .local_get(3)
            .i32_add()
            .i32_load8_u(MemArg {
                offset: 0,
                align: 0,
                memory_index: MEMORY_INDEX,
            })
            // b_ptr + i
            .local_get(1)
            .local_get(3)
            .i32_add()
            .i32_load8_u(MemArg {
                offset: 0,
                align: 0,
                memory_index: MEMORY_INDEX,
            })
            .i32_ne()
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end()
            // i = i + 1
            .local_get(3)
            .i32_const(1)
            .i32_add()
            .local_set(3)
            .br(0)
            .end() // close loop
            .end(); // close block

        // All bytes equal — return 1.
        i.i32_const(1).end();

        let idx =
            self.declare_function_with_body(&[ValType::I32, ValType::I32], &[ValType::I32], &body);
        self.str_eq = Some(idx);
        idx
    }

    /// Wasm function index of the `__str_eq` helper if it has been
    /// declared, else `None`.
    #[must_use]
    pub const fn str_eq_index(&self) -> Option<u32> {
        self.str_eq
    }

    /// Declare and emit the `__str_concat` runtime helper, returning
    /// its wasm function index. Subsequent calls return the same
    /// index. The bump allocator must already be declared (we
    /// re-declare it lazily here if needed since the helper calls
    /// it twice).
    ///
    /// Signature: `__str_concat(a: i32, b: i32) -> i32`. Each input
    /// is a string-header pointer; the result is a freshly-allocated
    /// header pointing at a freshly-allocated byte buffer that
    /// contains the concatenation of `a`'s bytes followed by `b`'s
    /// bytes.
    ///
    /// Implementation: allocate `a.len + b.len` bytes for the buffer,
    /// `memory.copy` each input into place, allocate an 8-byte header,
    /// store `(buffer_ptr, total_len)`, and return the header pointer.
    pub fn declare_str_concat(&mut self) -> u32 {
        if let Some(idx) = self.str_concat {
            return idx;
        }
        let alloc_idx = self.declare_bump_allocator();

        // Locals: 4 i32 — local 2 = a_len, 3 = b_len, 4 = total_len,
        // 5 = buffer_ptr, 6 = header_ptr. (Params live at 0/1.)
        let mut body = Function::new(core::iter::once((5, ValType::I32)));
        let len_mem_arg = MemArg {
            offset: u64::from(crate::layout::STRING_LEN_OFFSET),
            align: 2, // log2(4)
            memory_index: MEMORY_INDEX,
        };
        let ptr_mem_arg = MemArg {
            offset: u64::from(crate::layout::STRING_PTR_OFFSET),
            align: 2,
            memory_index: MEMORY_INDEX,
        };
        let header_mem_arg = |offset: u64| MemArg {
            offset,
            align: 2,
            memory_index: MEMORY_INDEX,
        };
        let mut i = body.instructions();

        // a_len = a.len
        i.local_get(0).i32_load(len_mem_arg).local_set(2);
        // b_len = b.len
        i.local_get(1).i32_load(len_mem_arg).local_set(3);

        // total_len = a_len + b_len
        i.local_get(2).local_get(3).i32_add().local_set(4);

        // buffer_ptr = __alloc(total_len)
        i.local_get(4).call(alloc_idx).local_set(5);

        // memory.copy(buffer_ptr, a.ptr, a_len)
        // wasm `memory.copy` takes [dest, src, n] on the stack with
        // dest pushed first.
        i.local_get(5)
            .local_get(0)
            .i32_load(ptr_mem_arg)
            .local_get(2)
            .memory_copy(MEMORY_INDEX, MEMORY_INDEX);

        // memory.copy(buffer_ptr + a_len, b.ptr, b_len)
        i.local_get(5)
            .local_get(2)
            .i32_add()
            .local_get(1)
            .i32_load(ptr_mem_arg)
            .local_get(3)
            .memory_copy(MEMORY_INDEX, MEMORY_INDEX);

        // header_ptr = __alloc(STRING_HEADER_SIZE)
        i.i32_const(i32::try_from(crate::layout::STRING_HEADER_SIZE).unwrap_or(8))
            .call(alloc_idx)
            .local_set(6);

        // header.ptr = buffer_ptr
        i.local_get(6)
            .local_get(5)
            .i32_store(header_mem_arg(u64::from(crate::layout::STRING_PTR_OFFSET)));
        // header.len = total_len
        i.local_get(6)
            .local_get(4)
            .i32_store(header_mem_arg(u64::from(crate::layout::STRING_LEN_OFFSET)));

        // Return header_ptr.
        i.local_get(6).end();

        let idx =
            self.declare_function_with_body(&[ValType::I32, ValType::I32], &[ValType::I32], &body);
        self.str_concat = Some(idx);
        idx
    }

    /// Wasm function index of the `__str_concat` helper if declared.
    #[must_use]
    pub const fn str_concat_index(&self) -> Option<u32> {
        self.str_concat
    }

    /// Declare and export `cabi_realloc`, the canonical-ABI hook the
    /// component runtime calls to allocate buffers in our linear
    /// memory before passing `string` / `list<T>` arguments in. The
    /// bump allocator is declared lazily if needed.
    ///
    /// Signature: `cabi_realloc(orig_ptr: i32, orig_size: i32, align:
    /// i32, new_size: i32) -> i32`. Our bump allocator can't actually
    /// resize an existing region, so we always hand back a fresh
    /// `__alloc(new_size)` allocation — the orig_* / align inputs are
    /// ignored. That's correct for the canonical-ABI cases we hit
    /// today (fresh allocations during string / list lowering): the
    /// host writes the bytes into the freshly-allocated region and
    /// then hands us the (ptr, len) pair.
    pub fn declare_cabi_realloc(&mut self) -> u32 {
        let alloc_idx = self.declare_bump_allocator();
        let mut body = Function::new(core::iter::empty());
        // Params: orig_ptr (0), orig_size (1), align (2), new_size (3).
        body.instructions().local_get(3).call(alloc_idx).end();
        let idx = self.declare_function_with_body(
            &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
            &[ValType::I32],
            &body,
        );
        self.export_function(CABI_REALLOC_NAME, idx);
        idx
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
        let heap_base = compute_heap_base(self.string_data.len());

        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(heap_base),
        );

        let mut data = DataSection::new();
        if !self.string_data.is_empty() {
            data.active(
                MEMORY_INDEX,
                &ConstExpr::i32_const(HEAP_BASE),
                self.string_data.iter().copied(),
            );
        }

        let mut module = Module::new();
        // Section order is fixed by the wasm spec: type, function,
        // table, memory, global, export, element, code, data.
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
        module.section(&globals);
        if !self.exports.is_empty() {
            module.section(&self.exports);
        }
        if !self.elements.is_empty() {
            module.section(&self.elements);
        }
        if !self.code.is_empty() {
            module.section(&self.code);
        }
        if !self.string_data.is_empty() {
            module.section(&data);
        }
        module.finish()
    }
}

/// Compute the bump-allocator's initial heap-pointer value given the
/// size of the static data segment.
///
/// The heap starts at [`HEAP_BASE`] when the data segment is empty
/// (the legacy default for tests that build modules without literals).
/// Once the data segment carries bytes, the heap pointer rounds up to
/// the bump allocator's alignment so the first allocation cannot
/// straddle the boundary.
fn compute_heap_base(data_len: usize) -> i32 {
    if data_len == 0 {
        return HEAP_BASE;
    }
    let raw = u32::try_from(data_len).unwrap_or(u32::MAX);
    let mask = BUMP_ALLOCATOR_ALIGN.saturating_sub(1);
    let aligned = raw.saturating_add(mask) & !mask;
    i32::try_from(aligned).unwrap_or(i32::MAX)
}
