//! Builder for the core WebAssembly module the backend assembles
//! during code generation.
//!
//! [`ModuleBuilder`] starts from an empty validating module that
//! already declares one linear memory and one mutable `i32` heap
//! pointer global. Subsequent phases plug additional sections (types,
//! functions, code, exports …) into the builder before
//! [`ModuleBuilder::finish`] emits the byte-encoded module.

use wasm_encoder::{
    CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection, GlobalSection,
    GlobalType, MemorySection, MemoryType, Module, TypeSection, ValType,
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
    memories: MemorySection,
    globals: GlobalSection,
    exports: ExportSection,
    code: CodeSection,
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

        Self {
            types: TypeSection::new(),
            functions: FunctionSection::new(),
            memories,
            globals,
            exports: ExportSection::new(),
            code: CodeSection::new(),
        }
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
        // Section order matters for validation: type, function, memory,
        // global, code.
        if !self.types.is_empty() {
            module.section(&self.types);
        }
        if !self.functions.is_empty() {
            module.section(&self.functions);
        }
        module.section(&self.memories);
        module.section(&self.globals);
        if !self.exports.is_empty() {
            module.section(&self.exports);
        }
        if !self.code.is_empty() {
            module.section(&self.code);
        }
        module.finish()
    }
}
