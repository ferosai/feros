//! Denoiser stage — wraps a denoiser backend for inline (non-actor) use.
//!
//! Called synchronously on every audio frame by the Reactor.
//! If the model is not loaded (disabled in config), passes audio through unchanged.

use crate::audio_ml::denoiser::dfn3::DeepFilterNetDenoiser;
use crate::audio_ml::denoiser::dtln::DtlnDenoiser;
use crate::audio_ml::denoiser::rnnoise::RnnoiseDenoiser;
use crate::audio_ml::denoiser::DenoiserBackend;
use tracing::warn;

enum Inner {
    Dtln(Box<DtlnDenoiser>),
    DeepFilterNet(Box<DeepFilterNetDenoiser>),
    RNNoise(Box<RnnoiseDenoiser>),
}

pub struct DenoiserStage {
    inner: Option<Inner>,
}

impl DenoiserStage {
    /// Create a denoiser stage. If `enabled` is false, audio passes through unchanged.
    pub fn new(models_dir: &str, enabled: bool, backend: DenoiserBackend) -> Self {
        if enabled {
            let inner = match backend {
                DenoiserBackend::Dtln => {
                    Inner::Dtln(Box::new(DtlnDenoiser::new(&format!("{}/dtln", models_dir))))
                }
                DenoiserBackend::DeepFilterNet3 => Inner::DeepFilterNet(Box::new(
                    DeepFilterNetDenoiser::new(&format!("{}/deepfilternet3", models_dir)),
                )),
                DenoiserBackend::RNNoise => Inner::RNNoise(Box::default()),
            };
            Self { inner: Some(inner) }
        } else {
            Self { inner: None }
        }
    }

    /// Initialize models. No-op if denoising is disabled.
    pub fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match &mut self.inner {
            Some(Inner::Dtln(d)) => d.initialize(),
            Some(Inner::DeepFilterNet(d)) => d.initialize(),
            Some(Inner::RNNoise(d)) => d.initialize(),
            None => Ok(()),
        }
    }

    /// Returns `true` when a denoiser model is active; `false` in passthrough mode.
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Process a single 16kHz PCM-16 LE audio frame.
    /// Returns cleaned audio (or the original if denoising is disabled/failed).
    pub fn process(&mut self, frame: &[u8]) -> Vec<u8> {
        match &mut self.inner {
            Some(Inner::Dtln(d)) => {
                let out = d.denoise_frame(frame);
                if out.is_empty() {
                    warn!("DenoiserStage[dtln] returned empty frame — passing through");
                    return frame.to_vec();
                }
                out
            }
            Some(Inner::DeepFilterNet(d)) => {
                let out = d.denoise_frame(frame);
                if out.is_empty() {
                    warn!("DenoiserStage[dfn3] returned empty frame — passing through");
                    return frame.to_vec();
                }
                out
            }
            Some(Inner::RNNoise(d)) => {
                let out = d.denoise_frame(frame);
                if out.is_empty() {
                    warn!("DenoiserStage[rnnoise] returned empty frame — passing through");
                    return frame.to_vec();
                }
                out
            }
            None => frame.to_vec(),
        }
    }
}
