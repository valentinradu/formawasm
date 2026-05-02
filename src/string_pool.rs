//! Compile-time string-literal pool feeding the wasm `data` section.
//!
//! Every `Literal::String` in the IR is interned through this pool
//! before any function body is lowered. The pool seeds the literal's
//! bytes plus an 8-byte `{ ptr, len }` header into a single contiguous
//! data segment that lives at offset 0 of linear memory. Each string's
//! header offset is what the runtime "value" of the literal points at;
//! the header's `ptr` slot in turn points at the bytes elsewhere in
//! the same segment.
//!
//! Deduplication is by source bytes — two `Literal::String` nodes
//! carrying `"hello"` share the same header. The bump allocator's
//! `HEAP_BASE` is relocated past the data section by
//! [`crate::module::ModuleBuilder::finish`] so heap allocations cannot
//! trample literals.

use std::collections::HashMap;

use thiserror::Error;

use crate::layout::{STRING_HEADER_ALIGN, STRING_HEADER_SIZE};

/// Errors produced by [`StringPool::intern`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StringPoolError {
    /// The pool would exceed `u32::MAX` bytes after this insertion.
    /// Linear-memory offsets are `u32` in core wasm so we cannot
    /// represent a data section larger than that.
    #[error("string pool size exceeds u32::MAX after interning a {len}-byte literal")]
    SizeOverflow {
        /// Length in bytes of the offending literal.
        len: usize,
    },
}

/// A growing data buffer plus a `text -> header_offset` map.
#[derive(Debug, Default, Clone)]
pub(crate) struct StringPool {
    data: Vec<u8>,
    by_text: HashMap<String, u32>,
}

impl StringPool {
    /// Build an empty pool.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Intern `text` and return the header's byte offset within the
    /// pool's data buffer. Subsequent calls with the same text return
    /// the same offset (no duplicate bytes).
    pub(crate) fn intern(&mut self, text: &str) -> Result<u32, StringPoolError> {
        if let Some(&offset) = self.by_text.get(text) {
            return Ok(offset);
        }

        // Step 1: append the literal's raw bytes at the next free
        // position. Their offset is what the header's `ptr` slot
        // stores.
        let bytes_offset = u32::try_from(self.data.len())
            .map_err(|_| StringPoolError::SizeOverflow { len: text.len() })?;
        self.data.extend_from_slice(text.as_bytes());

        // Step 2: pad up to the header's alignment so the i32 fields
        // we're about to write satisfy `STRING_HEADER_ALIGN`.
        let header_align = STRING_HEADER_ALIGN as usize;
        while !self.data.len().is_multiple_of(header_align) {
            self.data.push(0);
        }

        let header_offset = u32::try_from(self.data.len())
            .map_err(|_| StringPoolError::SizeOverflow { len: text.len() })?;

        // Step 3: append the 8-byte `{ ptr, len }` header.
        self.data.extend_from_slice(&bytes_offset.to_le_bytes());
        let len_bytes = u32::try_from(text.len())
            .map_err(|_| StringPoolError::SizeOverflow { len: text.len() })?;
        self.data.extend_from_slice(&len_bytes.to_le_bytes());

        let _ = STRING_HEADER_SIZE; // documents the 8 bytes just appended
        self.by_text.insert(text.to_owned(), header_offset);
        Ok(header_offset)
    }

    /// Borrow the data buffer for emission as a wasm `active` data
    /// segment at offset 0.
    #[must_use]
    pub(crate) fn data(&self) -> &[u8] {
        &self.data
    }

    /// Borrow the text-to-offset lookup map. Lowering threads this
    /// into [`crate::lower::LowerContext`] so `Literal::String`
    /// lowerings can resolve their header offsets without a mutable
    /// borrow on the pool.
    #[must_use]
    pub(crate) const fn lookup_map(&self) -> &HashMap<String, u32> {
        &self.by_text
    }
}
