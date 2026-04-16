//! VAD stage — wraps SileroVad for inline (non-actor) use.
//!
//! Called synchronously on every 16kHz 512-sample audio frame.
//! Returns a `VadEvent` when speech starts or ends, `None` otherwise.
//! Speech audio is accumulated inside `SileroVad` and yielded on SpeechEnd.

use bytes::Bytes;

use crate::audio_ml::vad::{SileroVad, VadConfig, VadEvent as InnerVadEvent};
use crate::types::VadEvent;

pub struct VadStage {
    inner: SileroVad,
}

impl VadStage {
    pub fn set_threshold(&mut self, threshold: f32) {
        // Safe to use != here: callers always supply named constants (VAD_THRESHOLD_*).
        // See the note in SileroVad::set_threshold if arithmetic thresholds are ever added.
        if self.inner.threshold() != threshold {
            self.inner.set_threshold(threshold);
        }
    }

    pub fn new(model_path: &str, config: VadConfig) -> Self {
        Self {
            inner: SileroVad::new(model_path, config),
        }
    }

    /// Initialize ONNX model.
    pub fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.initialize()
    }

    /// Process one 16kHz PCM-16 frame.
    /// Returns `Some(VadEvent)` on speech transitions, `None` otherwise.
    pub fn process(&mut self, frame: &[u8]) -> Option<VadEvent> {
        match self.inner.process_frame(frame) {
            Some(InnerVadEvent::SpeechStart) => Some(VadEvent::SpeechStarted),
            Some(InnerVadEvent::SpeechEnd) => {
                let audio = Bytes::from(self.inner.take_speech_audio());
                if audio.is_empty() {
                    None
                } else {
                    Some(VadEvent::SpeechEnded(audio))
                }
            }
            None => None,
        }
    }

    /// Force speech end (e.g. for `ForceSpeechEnd` events).
    #[allow(dead_code)]
    pub fn force_end(&mut self) -> Option<VadEvent> {
        if self.inner.is_speaking() {
            self.inner.force_speech_end();
            let audio = Bytes::from(self.inner.take_speech_audio());
            if audio.is_empty() {
                None
            } else {
                Some(VadEvent::SpeechEnded(audio))
            }
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn is_speaking(&self) -> bool {
        self.inner.is_speaking()
    }

    /// Reset all VAD state (e.g. at session start).
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}
