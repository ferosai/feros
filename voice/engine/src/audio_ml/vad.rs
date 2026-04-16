//! Silero VAD — ONNX Runtime inference for real-time voice activity detection.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tracing::{info, warn};

use crate::utils::FRAME_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    SpeechStart,
    SpeechEnd,
}

/// Default VAD threshold used when the bot is silent / listening.
pub const VAD_THRESHOLD_IDLE: f32 = 0.70;

/// VAD threshold used during bot playback on the standard Reactor path.
/// Audio is pre-filtered by the denoiser before reaching VAD, so 0.85 gives
/// meaningful noise suppression without requiring the user to shout to barge in.
pub const VAD_THRESHOLD_PLAYBACK: f32 = 0.85;

/// VAD threshold used during Gemini Live bot playback.
/// Higher than `VAD_THRESHOLD_PLAYBACK` because audio on this path is raw
/// (undenoised) — no denoiser pre-filters mic input before reaching VAD.
pub const VAD_THRESHOLD_PLAYBACK_RAW: f32 = 0.90;

#[derive(Debug, Clone)]
pub struct VadConfig {
    pub threshold: f32,
    pub min_volume: f32,
    pub silence_frames: u32,
    pub min_speech_frames: u32,
    pub lookback_frames: usize,
    pub context_size: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: VAD_THRESHOLD_IDLE,
            min_volume: 0.0035,
            silence_frames: 6,
            min_speech_frames: 6, // Increased from 3 (96ms) to 6 (192ms) to filter pops/echo
            lookback_frames: 20,
            context_size: 64,
        }
    }
}

struct OnnxState {
    shape: Vec<usize>,
    data: Vec<f32>,
}

pub struct SileroVad {
    session: Option<Session>,
    config: VadConfig,
    model_path: String,
    n_states: usize,
    states: Vec<OnnxState>,
    context: Vec<f32>,
    is_speaking: bool,
    speech_frame_count: u32,
    silence_frame_count: u32,
    lookback_buffer: Vec<Vec<u8>>,
    speech_audio: Vec<u8>,
    last_reset_time: std::time::Instant,
    volume: f32,
}

impl SileroVad {
    pub fn threshold(&self) -> f32 {
        self.config.threshold
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        // Equality is safe here: both sides always come from named constants
        // (VAD_THRESHOLD_*). If threshold is ever derived by arithmetic, switch
        // to an epsilon comparison to avoid IEEE 754 surprises.
        self.config.threshold = threshold;
    }

    pub fn new(model_path: &str, config: VadConfig) -> Self {
        let ctx = config.context_size;
        Self {
            session: None,
            config,
            model_path: model_path.to_string(),
            n_states: 0,
            states: Vec::new(),
            context: vec![0.0; ctx],
            is_speaking: false,
            speech_frame_count: 0,
            silence_frame_count: 0,
            lookback_buffer: Vec::new(),
            speech_audio: Vec::new(),
            last_reset_time: std::time::Instant::now(),
            volume: 0.0,
        }
    }

    pub fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.session.is_some() {
            return Ok(());
        }
        let path = Path::new(&self.model_path);
        if !path.exists() {
            return Err(format!("Silero VAD not found: {}", self.model_path).into());
        }

        let session = Session::builder()?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .commit_from_file(path)?;

        let inputs = session.inputs();
        let n = inputs.len();
        self.n_states = n.saturating_sub(2);

        self.states.clear();
        for inp in inputs.iter().skip(1).take(self.n_states) {
            let dims: Vec<usize> = if let Some(shape) = inp.dtype().tensor_shape() {
                shape
                    .iter()
                    .map(|&d| if d < 0 { 1 } else { d as usize })
                    .collect()
            } else {
                vec![1]
            };
            let n_elems: usize = dims.iter().product();
            info!("[vad] state '{}' shape {:?}", inp.name(), dims);
            self.states.push(OnnxState {
                shape: dims,
                data: vec![0.0f32; n_elems],
            });
        }

        info!("[vad] Loaded ({} state tensors)", self.n_states);
        self.session = Some(session);
        Ok(())
    }

    pub fn infer(&mut self, frame: &[u8]) -> Result<f32, Box<dyn std::error::Error + Send + Sync>> {
        let n_samples = frame.len() / 2;
        let audio_f32: Vec<f32> = (0..n_samples)
            .map(|i| i16::from_le_bytes([frame[i * 2], frame[i * 2 + 1]]) as f32 / 32768.0)
            .collect();

        let ctx = self.config.context_size;
        let alen = n_samples.min(FRAME_SIZE);
        let total = ctx + alen;
        let mut iv = Vec::with_capacity(total);
        iv.extend_from_slice(&self.context);
        iv.extend_from_slice(&audio_f32[..alen]);

        let input_t = Tensor::from_array(([1usize, total], iv.clone().into_boxed_slice()))?;
        let sr_t = Tensor::from_array(([1usize], vec![16000i64].into_boxed_slice()))?;

        let mut input_values: Vec<ort::session::SessionInputValue> = Vec::new();
        input_values.push(input_t.into());
        for state in &self.states {
            let st =
                Tensor::from_array((state.shape.clone(), state.data.clone().into_boxed_slice()))?;
            input_values.push(st.into());
        }
        input_values.push(sr_t.into());

        let session = self.session.as_mut().ok_or("VAD not init")?;
        let outputs = session.run(input_values.as_slice())?;

        let prob = outputs[0]
            .try_extract_tensor::<f32>()?
            .1
            .first()
            .copied()
            .unwrap_or(0.0);

        for (i, state) in self.states.iter_mut().enumerate() {
            if let Ok(t) = outputs[i + 1].try_extract_tensor::<f32>() {
                state.data = t.1.to_vec();
            }
        }

        if iv.len() >= ctx {
            self.context.copy_from_slice(&iv[iv.len() - ctx..]);
        }
        Ok(prob)
    }

    pub fn process_frame(&mut self, frame: &[u8]) -> Option<VadEvent> {
        // Periodic model reset to prevent hidden state drift
        if self.last_reset_time.elapsed().as_secs_f32() >= 5.0 && !self.is_speaking {
            self.reset_states();
        }

        // Calculate and smooth frame volume
        let n_samples = frame.len() / 2;
        let mut sum_sq = 0.0;
        for i in 0..n_samples {
            let sample = i16::from_le_bytes([frame[i * 2], frame[i * 2 + 1]]) as f32 / 32768.0;
            sum_sq += sample * sample;
        }
        let rms = (sum_sq / n_samples as f32).sqrt();
        self.volume = self.volume + 0.2 * (rms - self.volume);

        let prob = match self.infer(frame) {
            Ok(p) => p,
            Err(e) => {
                warn!("VAD err: {}", e);
                return None;
            }
        };

        let is_speech_pred = prob >= self.config.threshold && self.volume >= self.config.min_volume;

        if is_speech_pred {
            self.silence_frame_count = 0;
            if !self.is_speaking {
                self.speech_frame_count += 1;
                if self.speech_frame_count >= self.config.min_speech_frames {
                    self.is_speaking = true;
                    self.speech_frame_count = 0;
                    self.speech_audio.clear();
                    for lb in &self.lookback_buffer {
                        self.speech_audio.extend_from_slice(lb);
                    }
                    self.speech_audio.extend_from_slice(frame);
                    return Some(VadEvent::SpeechStart);
                }
            } else {
                self.speech_audio.extend_from_slice(frame);
            }
        } else {
            self.speech_frame_count = 0;
            if self.is_speaking {
                self.silence_frame_count += 1;
                self.speech_audio.extend_from_slice(frame);

                if self.silence_frame_count >= self.config.silence_frames {
                    self.is_speaking = false;
                    self.silence_frame_count = 0;
                    return Some(VadEvent::SpeechEnd);
                }
            }
        }

        if !self.is_speaking {
            self.lookback_buffer.push(frame.to_vec());
            if self.lookback_buffer.len() > self.config.lookback_frames {
                self.lookback_buffer.remove(0);
            }
        }
        None
    }

    pub fn get_speech_audio(&self) -> &[u8] {
        &self.speech_audio
    }

    pub fn take_speech_audio(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.speech_audio)
    }

    pub fn force_speech_end(&mut self) {
        self.is_speaking = false;
        self.silence_frame_count = 0;
    }

    pub fn reset_states(&mut self) {
        for s in &mut self.states {
            s.data.fill(0.0);
        }
        self.context.fill(0.0);
        self.last_reset_time = std::time::Instant::now();
    }

    pub fn reset(&mut self) {
        self.reset_states();
        self.is_speaking = false;
        self.speech_frame_count = 0;
        self.silence_frame_count = 0;
        self.lookback_buffer.clear();
        self.speech_audio.clear();
        self.volume = 0.0;
    }

    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    /// Drive the state machine with a given probability, bypassing ONNX.
    /// Useful for testing without the model file.
    pub fn process_with_prob(&mut self, prob: f32, frame: &[u8]) -> Option<VadEvent> {
        // Periodic model reset (uses elapsed time)
        if self.last_reset_time.elapsed().as_secs_f32() >= 5.0 && !self.is_speaking {
            self.reset_states();
        }

        // Volume from frame
        let n_samples = frame.len() / 2;
        let mut sum_sq = 0.0f32;
        for i in 0..n_samples {
            let sample = i16::from_le_bytes([frame[i * 2], frame[i * 2 + 1]]) as f32 / 32768.0;
            sum_sq += sample * sample;
        }
        let rms = (sum_sq / n_samples as f32).sqrt();
        self.volume = self.volume + 0.2 * (rms - self.volume);

        let is_speech_pred = prob >= self.config.threshold && self.volume >= self.config.min_volume;

        if is_speech_pred {
            self.silence_frame_count = 0;
            if !self.is_speaking {
                self.speech_frame_count += 1;
                if self.speech_frame_count >= self.config.min_speech_frames {
                    self.is_speaking = true;
                    self.speech_frame_count = 0;
                    self.speech_audio.clear();
                    for lb in &self.lookback_buffer {
                        self.speech_audio.extend_from_slice(lb);
                    }
                    self.speech_audio.extend_from_slice(frame);
                    return Some(VadEvent::SpeechStart);
                }
            } else {
                self.speech_audio.extend_from_slice(frame);
            }
        } else {
            self.speech_frame_count = 0;
            if self.is_speaking {
                self.silence_frame_count += 1;
                self.speech_audio.extend_from_slice(frame);

                if self.silence_frame_count >= self.config.silence_frames {
                    self.is_speaking = false;
                    self.silence_frame_count = 0;
                    return Some(VadEvent::SpeechEnd);
                }
            }
        }

        if !self.is_speaking {
            self.lookback_buffer.push(frame.to_vec());
            if self.lookback_buffer.len() > self.config.lookback_frames {
                self.lookback_buffer.remove(0);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic PCM-16 frame at the given RMS amplitude (0.0–1.0).
    /// 512 samples at 16kHz = 32ms, matching FRAME_SIZE.
    fn make_frame(amplitude: f32) -> Vec<u8> {
        let n = FRAME_SIZE;
        let sample = (amplitude * 32767.0) as i16;
        let mut frame = Vec::with_capacity(n * 2);
        for _ in 0..n {
            frame.extend_from_slice(&sample.to_le_bytes());
        }
        frame
    }

    fn make_vad(threshold: f32) -> SileroVad {
        SileroVad::new(
            "",
            VadConfig {
                threshold,
                min_volume: 0.0, // disable volume gate so tests focus on threshold
                silence_frames: 6,
                min_speech_frames: 1, // fire SpeechStart on the first positive frame
                lookback_frames: 0,
                context_size: 0,
            },
        )
    }

    /// prob=0.80 should fire SpeechStart at the idle threshold (0.70) but be
    /// treated as silence at the playback threshold (0.85).
    #[test]
    fn threshold_controls_speech_detection() {
        let frame = make_frame(0.1);

        // At idle threshold: 0.80 >= 0.70 → speech
        let mut vad = make_vad(VAD_THRESHOLD_IDLE);
        let result = vad.process_with_prob(0.80, &frame);
        assert_eq!(result, Some(VadEvent::SpeechStart));

        // At playback threshold: 0.80 < 0.85 → silence, no event
        let mut vad = make_vad(VAD_THRESHOLD_PLAYBACK);
        let result = vad.process_with_prob(0.80, &frame);
        assert_eq!(result, None);
    }

    /// set_threshold mid-stream updates the comparison boundary immediately.
    #[test]
    fn set_threshold_takes_effect_immediately() {
        let frame = make_frame(0.1);
        let mut vad = make_vad(VAD_THRESHOLD_IDLE);

        // Prime with sub-threshold prob so is_speaking stays false
        vad.process_with_prob(0.50, &frame);
        assert!(!vad.is_speaking());

        // Elevate to playback threshold — 0.80 should now be below the gate
        vad.set_threshold(VAD_THRESHOLD_PLAYBACK);
        let result = vad.process_with_prob(0.80, &frame);
        assert_eq!(
            result, None,
            "prob 0.80 should be below playback threshold 0.85"
        );

        // Drop back to idle — same prob should now trigger
        vad.set_threshold(VAD_THRESHOLD_IDLE);
        let result = vad.process_with_prob(0.80, &frame);
        assert_eq!(result, Some(VadEvent::SpeechStart));
    }
}
