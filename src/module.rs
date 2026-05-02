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
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    MemArg, MemorySection, MemoryType, Module, NameMap, NameSection, RefType, TableSection,
    TableType, TypeSection, ValType,
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

/// Wasm-import module name `wit-component` lifts world-level imports under.
///
/// Per the canonical-ABI 32-bit-platform-2 mangling (see
/// `wit-component`'s `validation::Standard`), each
/// `extern_abi`-bearing function in the IR module maps to one
/// `(IMPORT_MODULE_NAME, kebab-case-fn-name)` core-wasm import.
pub const IMPORT_MODULE_NAME: &str = "cm32p2";

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
    imports: ImportSection,
    /// Number of imported functions declared so far. Local function
    /// indices start at this offset, since imports occupy the leading
    /// region of the wasm function-index space.
    import_function_count: u32,
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
    /// Index of the funcref `Table` carrying every trait-method
    /// function reachable through a vtable. Created lazily by
    /// [`Self::declare_method_table`]; populated via
    /// [`Self::populate_method_table`].
    method_table_idx: Option<u32>,
    /// Static data segment carrying string-literal bytes + headers
    /// followed by per-impl vtable blobs. Lives at offset 0 in linear
    /// memory; the bump-allocator's heap starts immediately after this
    /// region.
    static_data: Vec<u8>,
    /// Source-level function names indexed by wasm function index.
    /// Populated incrementally via [`Self::set_function_name`] and
    /// emitted as a `name` custom section in [`Self::finish`] so
    /// debug tooling sees the original identifiers instead of
    /// `func[N]`. `None` slots stay anonymous in the output —
    /// expected for test fixtures that build modules through the
    /// raw `declare_function*` API without going through
    /// `module_lowering`.
    function_names: Vec<Option<String>>,
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
            imports: ImportSection::new(),
            import_function_count: 0,
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
            method_table_idx: None,
            static_data: Vec::new(),
            function_names: Vec::new(),
        }
    }

    /// Record a source-level name for the wasm function at
    /// `function_index`. Names land in the `name` custom section
    /// emitted at [`Self::finish`] time and let debug tooling
    /// (`wasmtime --debug`, browser devtools, `wasm-tools print`)
    /// show readable identifiers instead of `func[N]`. Calling for
    /// an index past the current end-of-vector grows the names
    /// vector; any unwritten slot stays anonymous.
    pub fn set_function_name(&mut self, function_index: u32, name: &str) {
        let idx = function_index as usize;
        if self.function_names.len() <= idx {
            self.function_names.resize(idx.saturating_add(1), None);
        }
        if let Some(slot) = self.function_names.get_mut(idx) {
            *slot = Some(name.to_owned());
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
    ///
    /// Module-lowering builds this buffer by concatenating the string
    /// pool's bytes (at offset 0) with any per-impl vtable blobs. Both
    /// regions are read-only static data — strings keep header/byte
    /// offsets within the buffer; vtables store funcref-table indices
    /// that virtual-dispatch lowering loads at the call site.
    pub fn set_static_data(&mut self, bytes: Vec<u8>) {
        self.static_data = bytes;
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

    /// Lazy-create the funcref `Table` of `num_methods` slots used by
    /// virtual trait-method dispatch. Returns the table index. Calling
    /// repeatedly returns the original index without re-declaring.
    pub fn declare_method_table(&mut self, num_methods: u32) -> u32 {
        if let Some(idx) = self.method_table_idx {
            return idx;
        }
        let idx = self.tables.len();
        self.tables.table(TableType {
            element_type: RefType::FUNCREF,
            minimum: u64::from(num_methods),
            maximum: Some(u64::from(num_methods)),
            table64: false,
            shared: false,
        });
        self.method_table_idx = Some(idx);
        idx
    }

    /// Populate the method funcref table with `wasm_func_indices`, in
    /// order, starting at element offset 0. The vtable slot value
    /// stored in linear memory is the slot number (the table's element
    /// index), which `call_indirect` resolves to the function pointer
    /// at runtime.
    pub fn populate_method_table(&mut self, wasm_func_indices: &[u32]) {
        let Some(table_idx) = self.method_table_idx else {
            return;
        };
        self.elements.active(
            Some(table_idx),
            &ConstExpr::i32_const(0),
            Elements::Functions(Cow::Borrowed(wasm_func_indices)),
        );
    }

    /// Wasm table index of the method funcref table if declared.
    #[must_use]
    pub const fn method_table_index(&self) -> Option<u32> {
        self.method_table_idx
    }

    /// Export a previously-declared function under `name`.
    pub fn export_function(&mut self, name: &str, function_index: u32) {
        self.exports.export(name, ExportKind::Func, function_index);
    }

    /// Declare a function with the given param + result valtypes and a
    /// caller-supplied body. The body must already include the closing
    /// `end` instruction. Returns the wasm function index — accounting
    /// for the imported-function region that occupies the leading
    /// indices of the wasm function-index space.
    pub fn declare_function_with_body(
        &mut self,
        param_types: &[ValType],
        result_types: &[ValType],
        body: &Function,
    ) -> u32 {
        let type_index = self.types.len();
        let local_index = self.functions.len();
        let func_index = self.import_function_count.saturating_add(local_index);

        self.types
            .ty()
            .function(param_types.iter().copied(), result_types.iter().copied());
        self.functions.function(type_index);
        self.code.function(body);

        func_index
    }

    /// Declare a wasm function import under
    /// `(module_name, fn_name)` with the given signature, and
    /// return the wasm function index assigned to it.
    ///
    /// Imports occupy the leading region of the wasm function-index
    /// space — the spec orders the import section before the
    /// function section. Every call to this method must therefore
    /// happen before any [`Self::declare_function_with_body`] call
    /// whose returned index the caller intends to compare against an
    /// import's; the helper itself bumps `import_function_count` so
    /// subsequent locally-defined functions land at the right offset.
    pub fn declare_function_import(
        &mut self,
        module_name: &str,
        fn_name: &str,
        param_types: &[ValType],
        result_types: &[ValType],
    ) -> u32 {
        let type_index = self.types.len();
        self.types
            .ty()
            .function(param_types.iter().copied(), result_types.iter().copied());
        self.imports
            .import(module_name, fn_name, EntityType::Function(type_index));
        let func_index = self.import_function_count;
        self.import_function_count = self.import_function_count.saturating_add(1);
        // Mirror the import's wire name into the `name` section so
        // host-provided imports show up as `host-double` rather than
        // `func[0]` in disassembly.
        self.set_function_name(func_index, fn_name);
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
        self.set_function_name(idx, BUMP_ALLOCATOR_NAME);
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
        self.set_function_name(idx, STR_EQ_NAME);
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
        self.set_function_name(idx, STR_CONCAT_NAME);
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
        self.set_function_name(idx, CABI_REALLOC_NAME);
        self.export_function(CABI_REALLOC_NAME, idx);
        idx
    }

    /// Total wasm function-index-space size — imports plus locally-
    /// defined functions. The wasm spec puts imports first, so this
    /// is `import_function_count + functions.len()`.
    #[must_use]
    pub fn function_count(&self) -> u32 {
        self.import_function_count
            .saturating_add(self.functions.len())
    }

    /// Encode the module as a sequence of bytes ready for validation
    /// or component wrapping.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let heap_base = compute_heap_base(self.static_data.len());

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
        if !self.static_data.is_empty() {
            data.active(
                MEMORY_INDEX,
                &ConstExpr::i32_const(HEAP_BASE),
                self.static_data.iter().copied(),
            );
        }

        let mut module = Module::new();
        // Section order is fixed by the wasm spec: type, import,
        // function, table, memory, global, export, element, code,
        // data.
        if !self.types.is_empty() {
            module.section(&self.types);
        }
        if !self.imports.is_empty() {
            module.section(&self.imports);
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
        if !self.static_data.is_empty() {
            module.section(&data);
        }
        // Custom `name` section, last in the module so debug
        // tooling can read it without affecting any other
        // section's offsets. Only emitted when at least one
        // function carries a name — keeps test fixtures that build
        // anonymous modules byte-identical to before.
        if let Some(name_section) = build_name_section(&self.function_names) {
            module.section(&name_section);
        }
        module.finish()
    }
}

/// Assemble a `name` custom section from `function_names`. Returns
/// `None` if every slot is anonymous — emitting an empty
/// `NameSection` is wasteful and surfaces as zero-byte custom
/// section in `wasm-tools print` output.
fn build_name_section(function_names: &[Option<String>]) -> Option<NameSection> {
    if function_names.iter().all(Option::is_none) {
        return None;
    }
    let mut names = NameMap::new();
    for (idx, slot) in function_names.iter().enumerate() {
        if let Some(name) = slot {
            // Wasm function indices are u32; the `function_names`
            // vec is sized in lockstep with the function-index
            // space, so this conversion is exact for any module
            // the rest of the builder accepts.
            let idx_u32 = u32::try_from(idx).unwrap_or(u32::MAX);
            names.append(idx_u32, name);
        }
    }
    let mut section = NameSection::new();
    section.functions(&names);
    Some(section)
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
