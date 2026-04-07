//! `voice-trace` — unified event bus for the voice engine.
//!
//! All events the engine produces (state changes, transcripts, tool activity,
//! audio, metrics, raw traces) flow through a single [`EventBus`]. Consumers
//! subscribe with category-based filtering — see [`EventCategory`] docs for
//! the full subscriber-to-category mapping.
//!
//! Two methods emit to the bus:
//!
//! - **`trace(label)`** — emits `Event::Trace` with a monotonic seq and
//!   microsecond timestamp. Use for meaningful state transitions.
//! - **`emit(event)`** — emits any structured [`Event`] variant directly.
//!
//! # Usage
//!
//! ```ignore
//! use voice_trace::{Tracer, Event};
//!
//! let mut tracer = Tracer::new();
//!
//! // Meaningful state transition (emits Event::Trace on bus)
//! tracer.trace("BargeIn");
//!
//! // Structured event
//! tracer.emit(Event::StateChanged { state: "listening".into() });
//!
//! // Subscribe
//! let mut rx = tracer.subscribe();
//! ```
//!
//! # Feature Flags
//!
//! | Feature | Description |
//! |---|---|
//! | `otel` | Enable OpenTelemetry export (OTLP spans to Datadog, Honeycomb, etc.) |
//! | `langfuse` | Enable Langfuse OTLP export for AI product analytics |
//! | `opus` | Enable OGG/Opus encoding (vendored libopus, no system lib needed). Default off; falls back to WAV. |

pub mod bus;
pub mod event;
pub mod sink_event;
pub mod turn_tracker;
pub mod recording;

pub mod sinks;
pub mod tracer;

#[cfg(feature = "s3")]
pub mod s3_store;


// ── Re-exports ──────────────────────────────────────────────────

pub use bus::{EventBus, FilteredReceiver};
pub use event::{Event, EventCategory, LlmCompletionData};
pub use turn_tracker::{TurnMetrics, TurnTracker};
pub use sink_event::{from_event as to_sink_event, SinkEvent};
pub use recording::{spawn_recording_subscriber, RecordingOutput};

pub use tracer::Tracer;
