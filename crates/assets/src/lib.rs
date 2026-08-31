//! Application assets for Coop.
//!
//! ## Platform differences
//!
//! - **Native (desktop)**: assets are embedded into the binary at compile time
//!   with `rust-embed`.
//! - **WASM (web)**: assets are downloaded on demand from `{endpoint}/assets/{path}`
//!   and cached in memory. This keeps the WASM bundle size small.

#[cfg(not(target_family = "wasm"))]
mod native_assets;

#[cfg(target_family = "wasm")]
mod wasm_assets;

#[cfg(not(target_family = "wasm"))]
pub use native_assets::Assets;
#[cfg(target_family = "wasm")]
pub use wasm_assets::Assets;
