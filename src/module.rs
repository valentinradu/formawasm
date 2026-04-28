//! Builder for the core WebAssembly module the backend assembles
//! during code generation.
//!
//! [`ModuleBuilder`] starts from an empty validating module that
//! already declares one linear memory and one mutable `i32` heap
//! pointer global. Subsequent phases plug additional sections (types,
//! functions, code, exports …) into the builder before
//! [`ModuleBuilder::finish`] emits the byte-encoded module.

use wasm_encoder::{
    ConstExpr, GlobalSection, GlobalType, MemorySection, MemoryType, Module, ValType,
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
    memories: MemorySection,
    globals: GlobalSection,
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

        Self { memories, globals }
    }

    /// Encode the module as a sequence of bytes ready for validation
    /// or component wrapping.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let mut module = Module::new();
        module.section(&self.memories);
        module.section(&self.globals);
        module.finish()
    }
}
