//! Voice Engine library — shared modules for the voice pipeline.

// ── Reactor Architecture ─────────────────────────────────────────
pub mod reactor;
pub mod types;

// ── ONNX inference implementations ────────────────────────────────
pub mod audio_ml;

// ── Core modules ────────────────────────────────────────────────
pub mod language_config;
pub mod utils;

// Policies live inside the reactor module (colocated with reactor code).
// Re-export at crate root so existing `voice_engine::policies::*` paths still work.
pub use reactor::policies;

pub(crate) mod native_session;
pub mod providers;
#[cfg(feature = "pyo3")]
pub mod python;
pub mod server;
pub mod session;
pub mod settings;
