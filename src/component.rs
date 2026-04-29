//! Component-Model wrap layer.
//!
//! [`wrap_component`] takes the core-Wasm bytes produced by
//! [`crate::lower_module`] together with the WIT source produced by
//! [`crate::emit_wit`], embeds the corresponding world identity into
//! the core module's `component-type` custom section, and runs
//! [`wit_component::ComponentEncoder`] to emit a final Component-Model
//! artifact.
//!
//! The WIT must declare exactly one world named [`crate::wit::WORLD_NAME`]
//! — that's the contract `emit_wit` produces and `wrap_component`
//! consumes.

use thiserror::Error;
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

use crate::wit::WORLD_NAME;

/// Errors produced by [`wrap_component`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ComponentWrapError {
    /// The supplied WIT string failed to parse via `wit-parser`.
    /// Should not happen for strings produced by [`crate::emit_wit`],
    /// which round-trips its own output before returning.
    #[error("WIT failed to parse: {reason}")]
    WitParse {
        /// `wit-parser`'s diagnostic rendered as a string.
        reason: String,
    },

    /// The supplied WIT did not declare a world named
    /// [`crate::wit::WORLD_NAME`].
    #[error("WIT does not declare a world named '{WORLD_NAME}': {reason}")]
    WorldMissing {
        /// `wit-parser`'s diagnostic rendered as a string.
        reason: String,
    },

    /// `wit_component::embed_component_metadata` failed to attach the
    /// custom section to the core module.
    #[error("failed to embed component-type metadata into core module: {reason}")]
    EmbedFailed {
        /// `wit-component`'s diagnostic rendered as a string.
        reason: String,
    },

    /// `ComponentEncoder::module` rejected the core-Wasm bytes — most
    /// likely because the embedded metadata refers to exports the
    /// module does not actually provide.
    #[error("ComponentEncoder rejected the core module: {reason}")]
    EncoderRejected {
        /// `wit-component`'s diagnostic rendered as a string.
        reason: String,
    },

    /// `ComponentEncoder::encode` produced no bytes — the encoder ran
    /// to completion but the result failed component-model validation.
    #[error("ComponentEncoder failed to encode the component: {reason}")]
    EncodeFailed {
        /// `wit-component`'s diagnostic rendered as a string.
        reason: String,
    },
}

/// Wrap `core_bytes` into a Component-Model artifact.
///
/// `core_bytes` is consumed because `wit_component::embed_component_metadata`
/// appends the custom section in place. `wit` must declare exactly
/// one world named [`crate::wit::WORLD_NAME`].
pub fn wrap_component(mut core_bytes: Vec<u8>, wit: &str) -> Result<Vec<u8>, ComponentWrapError> {
    let mut resolve = Resolve::default();
    let pkg = resolve
        .push_str("formawasm-generated.wit", wit)
        .map_err(|e| ComponentWrapError::WitParse {
            reason: format!("{e:#}"),
        })?;
    let world = resolve
        .select_world(&[pkg], Some(WORLD_NAME))
        .map_err(|e| ComponentWrapError::WorldMissing {
            reason: format!("{e:#}"),
        })?;

    embed_component_metadata(&mut core_bytes, &resolve, world, StringEncoding::UTF8).map_err(
        |e| ComponentWrapError::EmbedFailed {
            reason: format!("{e:#}"),
        },
    )?;

    let mut encoder = ComponentEncoder::default()
        .validate(true)
        .module(&core_bytes)
        .map_err(|e| ComponentWrapError::EncoderRejected {
            reason: format!("{e:#}"),
        })?;
    encoder
        .encode()
        .map_err(|e| ComponentWrapError::EncodeFailed {
            reason: format!("{e:#}"),
        })
}
