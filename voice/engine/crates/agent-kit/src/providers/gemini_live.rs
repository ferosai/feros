//! Gemini Live (Multimodal Bidirectional WebSocket) provider.
//!
//! This provider implements the `RealtimeProvider` trait, establishing a persistent
//! WebSocket connection to Google's Gemini Live API for native audio-to-audio voice
//! conversations. Unlike the standard text-based `LlmProvider`, this bypasses both
//! the STT and TTS stages, with Gemini consuming and emitting raw PCM audio directly.
//!
//! # Architecture
//!
//! ```text
//! WebRTC PCM → push_user_audio() → BidiGenerateContentRealtimeInput →──┐
//!                                                                        │ ws
//! WebRTC speaker ← RealtimeEvent::BotAudioChunk ← recv_event() ←───────┘
//!                                                        ↓
//!                                     Also yields ToolCall, Transcription
//! ```
//!
//! # Session Resumption
//!
//! The provider saves the `session_resumption_handle` from Google's
//! `SessionResumptionUpdate` messages. On reconnect, this handle is passed in the
//! `BidiGenerateContentSetup` to restore context without re-uploading history.
//!
//! # References
//! - https://ai.google.dev/api/multimodal-live (Gemini Live API)
//! - https://ai.google.dev/api/multimodal-live#client-messages

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use super::LlmProviderError;
use super::realtime::{RealtimeConfig, RealtimeEvent, RealtimeProvider, VadState};

// ── Constants ───────────────────────────────────────────────────────────────

/// Gemini Live WebSocket endpoint.
const GEMINI_LIVE_WS_BASE: &str =
    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

/// Gemini Live input and output sample rate (PCM, mono, 16-bit).
pub const INPUT_SAMPLE_RATE: u32 = 16_000;
pub const OUTPUT_SAMPLE_RATE: u32 = 24_000;

/// Timeout for connecting to the Gemini backend.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);


// ── Wire protocol types ──────────────────────────────────────────────────────
// Mirrors the JSON shapes documented at https://ai.google.dev/api/multimodal-live

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BidiSetupMessage {
    setup: BidiGenerateContentSetup,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BidiGenerateContentSetup {
    model: String,
    generation_config: GenerationConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_resumption: Option<SessionResumptionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_audio_transcription: Option<AudioTranscriptionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_audio_transcription: Option<AudioTranscriptionConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    response_modalities: Vec<String>,
    speech_config: SpeechConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeechConfig {
    voice_config: VoiceConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceConfig {
    prebuilt_voice_config: PrebuiltVoiceConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrebuiltVoiceConfig {
    voice_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemInstruction {
    parts: Vec<TextPart>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextPart {
    text: String,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct AudioTranscriptionConfig {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResumptionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<String>,
}

// ── Outbound messages ────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RealtimeInputMessage {
    realtime_input: RealtimeInputPayload,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct RealtimeInputPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<BlobPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_start: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_end: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlobPayload {
    data: String, // base64-encoded PCM
    mime_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResponseMessage {
    tool_response: ToolResponsePayload,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResponsePayload {
    function_responses: Vec<FunctionResponse>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FunctionResponse {
    id: String,
    name: String,
    response: Value,
}


// ── Inbound messages ─────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ServerMessage {
    setup_complete: Option<Value>,
    server_content: Option<ServerContent>,
    tool_call: Option<ToolCallMessage>,
    session_resumption_update: Option<SessionResumptionUpdate>,
    /// Token usage emitted by Gemini at turn_complete boundaries.
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ServerContent {
    model_turn: Option<ModelTurn>,
    turn_complete: Option<bool>,
    interrupted: Option<bool>,
    input_transcription: Option<Transcription>,
    output_transcription: Option<Transcription>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ModelTurn {
    parts: Vec<ModelPart>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ModelPart {
    text: Option<String>,
    inline_data: Option<InlineData>,
    thought: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct InlineData {
    data: String, // base64-encoded
    mime_type: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Transcription {
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ToolCallMessage {
    function_calls: Vec<FunctionCall>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct FunctionCall {
    id: Option<String>,
    name: String,
    args: Value,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SessionResumptionUpdate {
    new_handle: Option<String>,
    resumable: Option<bool>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    prompt_token_count: Option<u32>,
    response_token_count: Option<u32>,
    #[allow(dead_code)]
    total_token_count: Option<u32>,
}

// ── Provider ─────────────────────────────────────────────────────────────────

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Gemini Live (Multimodal Bidirectional WebSocket) provider.
///
/// Maintains a single persistent WebSocket connection to the Gemini Live API.
/// Push raw PCM audio using `push_user_audio`, receive audio/tool/transcription
/// events via `recv_event`, and respond to tools via `send_tool_result`.
pub struct GeminiLiveProvider {
    api_key: String,
    model: String,
    session_resumption_handle: Option<String>,
    ws: Option<WsStream>,
    /// Accumulates fragmented `input_transcription` chunks from the Gemini wire
    /// protocol. Gemini streams transcription word-by-word, so we buffer until
    /// we see end-of-sentence punctuation (`.`, `?`, `!`) or a `TurnComplete`
    /// flushes the remainder.
    input_transcript_buf: String,
    /// Accumulates fragmented `output_transcription` chunks from the Gemini wire
    /// protocol so that UI logs are not fragmented by emitting per chunk.
    output_transcript_buf: String,
    /// Pending token counts from the last `usage_metadata` frame, consumed
    /// when `TurnComplete` is emitted so callers can update billing metrics.
    pending_usage: Option<UsageMetadata>,
    /// Pending events queue to handle multiple items from a single frame
    pending_events: std::collections::VecDeque<RealtimeEvent>,
}

impl GeminiLiveProvider {
    /// Create a new provider with the supplied API key and model name.
    ///
    /// Default model used for audio-to-audio:
    /// `"models/gemini-3.1-flash-live-preview"`
    pub fn new(api_key: String, model: Option<String>) -> Self {
        let model_name = match model {
            Some(m) if !m.is_empty() => {
                // Ensure the required "models/" prefix is present.
                if m.starts_with("models/") {
                    m
                } else {
                    format!("models/{m}")
                }
            }
            _ => {
                warn!("[gemini-live] Missing model specified — forcing fallback to gemini-3.1-flash-live-preview");
                "models/gemini-3.1-flash-live-preview".to_string()
            }
        };

        Self {
            api_key,
            model: model_name,
            session_resumption_handle: None,
            ws: None,
            input_transcript_buf: String::new(),
            output_transcript_buf: String::new(),
            pending_usage: None,
            pending_events: std::collections::VecDeque::new(),
        }
    }

    /// Build the WebSocket URL with the API key query parameter.
    fn ws_url(&self) -> String {
        format!("{}?key={}", GEMINI_LIVE_WS_BASE, self.api_key)
    }

    /// Build and send the initial `BidiGenerateContentSetup` message.
    async fn send_setup(&mut self, config: &RealtimeConfig) -> Result<(), LlmProviderError> {
        let ws = self.ws.as_mut().ok_or_else(|| {
            LlmProviderError::Transport("WebSocket not connected".to_string())
        })?;

        let model_name = if self.model.starts_with("models/") {
            self.model.clone()
        } else {
            format!("models/{}", self.model)
        };

        // Helper to convert OpenAI lowercase types ("string", "object") to Gemini uppercase ("STRING", "OBJECT")
        fn uppercase_schema_types(val: &mut serde_json::Value) {
            match val {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::String(s)) = map.get_mut("type") {
                        *s = s.to_ascii_uppercase();
                    }
                    for value in map.values_mut() {
                        uppercase_schema_types(value);
                    }
                }
                serde_json::Value::Array(arr) => {
                    for value in arr.iter_mut() {
                        uppercase_schema_types(value);
                    }
                }
                _ => {}
            }
        }

        let gemini_tools = config.tools.as_ref().map(|tools| {
            let mut function_declarations = Vec::new();
            for t in tools {
                let mut func = if let Some(f) = t.get("function") {
                    f.clone()
                } else if t.get("name").is_some() {
                    t.clone()
                } else {
                    continue;
                };

                // Convert type enums to uppercase for Gemini Schema format
                uppercase_schema_types(&mut func);
                function_declarations.push(func);
            }

            if function_declarations.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!([{
                    "functionDeclarations": function_declarations
                }])
            }
        }).filter(|v| !v.is_null()).and_then(|v| serde_json::from_value(v).ok());

        let setup = BidiSetupMessage {
            setup: BidiGenerateContentSetup {
                model: model_name,
                generation_config: GenerationConfig {
                    response_modalities: vec!["AUDIO".to_string()],
                    speech_config: SpeechConfig {
                        voice_config: VoiceConfig {
                            prebuilt_voice_config: PrebuiltVoiceConfig {
                                voice_name: config.voice_id.clone(),
                            },
                        },
                    },
                },
                system_instruction: config.system_instruction.as_ref().filter(|s| !s.is_empty()).map(|s| {
                    SystemInstruction {
                        parts: vec![TextPart { text: s.clone() }],
                    }
                }),
                tools: gemini_tools,
                session_resumption: self.session_resumption_handle.as_ref().map(|h| {
                    SessionResumptionConfig {
                        handle: Some(h.clone()),
                    }
                }),
                input_audio_transcription: None,
                output_audio_transcription: None,
            },
        };

        let json = serde_json::to_string(&setup)
            .map_err(|e| LlmProviderError::Provider(format!("setup serialization: {e}")))?;

        ws.send(Message::Text(json.into()))
            .await
            .map_err(|e| LlmProviderError::Transport(format!("setup send: {e}")))?;
        Ok(())
    }

    /// Convert `i16` PCM samples to bytes then base64.
    fn pcm_to_base64(samples: &[i16]) -> String {
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        BASE64.encode(&bytes)
    }

    /// Decode base64 audio from Gemini `inline_data` → `i16` PCM vector.
    fn base64_to_pcm(data: &str) -> Result<Vec<i16>, LlmProviderError> {
        let bytes = BASE64
            .decode(data)
            .map_err(|e| LlmProviderError::Provider(format!("base64 decode: {e}")))?;
        if bytes.len() % 2 != 0 {
            return Err(LlmProviderError::Provider(
                "PCM byte length is odd".to_string(),
            ));
        }
        Ok(bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect())
    }

    /// Poll the WebSocket for the next message and parse it.
    async fn next_server_message(&mut self) -> Result<ServerMessage, LlmProviderError> {
        loop {
            // Pull one frame from the WebSocket.
            let msg = {
                let ws = self.ws.as_mut().ok_or_else(|| {
                    LlmProviderError::Transport("WebSocket not connected".to_string())
                })?;
                ws.next()
                    .await
                    .ok_or_else(|| {
                        LlmProviderError::Transport("WebSocket stream closed".to_string())
                    })?
                    .map_err(|e| LlmProviderError::Transport(format!("ws recv: {e}")))?
            }; // ← ws borrow ends here, allowing us to call self.ws.as_mut() again for Pong

            match msg {
                Message::Text(text) => {
                    debug!("[gemini-live] recv: {} bytes", text.len());
                    return serde_json::from_str(&text).map_err(|e| {
                        LlmProviderError::Provider(format!(
                            "JSON parse error: {e}. Message: {}",
                            &text.chars().take(200).collect::<String>()
                        ))
                    });
                }
                Message::Binary(bin) => {
                    debug!("[gemini-live] recv binary: {} bytes", bin.len());
                    return serde_json::from_slice(&bin).map_err(|e| {
                        LlmProviderError::Provider(format!("binary JSON parse: {e}"))
                    });
                }
                Message::Ping(payload) => {
                    // ws borrow already dropped above — safe to re-borrow.
                    if let Some(ws) = self.ws.as_mut() {
                        let _ = ws.send(Message::Pong(payload)).await;
                    }
                    // Continue the loop to read the next frame.
                }
                Message::Close(frame) => {
                    tracing::warn!("[gemini-live] WebSocket closed by server: {:?}", frame);
                    return Err(LlmProviderError::Transport(
                        format!("WebSocket closed by server: {:?}", frame)
                    ));
                }
                _ => {} // Pong/Frame — ignore, loop again
            }
        }
    }

    /// Convert a `ServerMessage` into a list of `RealtimeEvent` items.
    /// Returns empty when the message contains no actionable data (e.g., usage-only).
    fn parse_server_message(&mut self, msg: ServerMessage) -> Vec<RealtimeEvent> {
        let mut events = Vec::new();

        // ── Session resumption handle ────────────────────────────────
        if let Some(update) = msg.session_resumption_update {
            if matches!(update.resumable, Some(true)) {
                if let Some(handle) = update.new_handle {
                    debug!("[gemini-live] New resumption handle: {}", &handle[..handle.len().min(16)]);
                    self.session_resumption_handle = Some(handle);
                }
            }
        }

        // ── Token usage ──────────────────────────────────────────────
        // Stash it so TurnComplete can carry it to callers.
        if let Some(usage) = msg.usage_metadata {
            self.pending_usage = Some(usage);
        }

        // ── Tool calls ───────────────────────────────────────────────
        if let Some(tool_call) = msg.tool_call {
            for fc in tool_call.function_calls {
                let call_id = fc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                events.push(RealtimeEvent::ToolCall {
                    call_id,
                    name: fc.name,
                    arguments: serde_json::to_string(&fc.args).unwrap_or_default(),
                });
            }
        }

        if let Some(sc) = msg.server_content {
            // ── Input (user) transcription ───────────────────────────
            // Gemini streams transcription word-by-word. Accumulate chunks and
            // only surface a full InputTranscription event when we see an
            // end-of-sentence marker (. ? !).
            // The buffer is also flushed unconditionally on TurnComplete below.
            if let Some(it) = sc.input_transcription {
                if let Some(text) = it.text.filter(|t| !t.is_empty()) {
                    // Leading space is cosmetic noise when the buffer is empty.
                    let chunk = if self.input_transcript_buf.is_empty() {
                        text.trim_start().to_string()
                    } else {
                        text
                    };
                    self.input_transcript_buf.push_str(&chunk);

                    // Flush on end-of-sentence punctuation.
                    if let Some(pos) = self.input_transcript_buf
                        .rfind(|c| matches!(c, '.' | '?' | '!'))
                    {
                        let sentence = self.input_transcript_buf[..=pos].trim().to_string();
                        self.input_transcript_buf = self.input_transcript_buf[pos + 1..].trim_start().to_string();
                        if !sentence.is_empty() {
                            events.push(RealtimeEvent::InputTranscription(sentence));
                        }
                    }
                    // Otherwise keep buffering — return nothing yet.
                }
            }

            // ── Output (bot) transcription ───────────────────────────
            if let Some(ot) = sc.output_transcription {
                if let Some(text) = ot.text.filter(|t| !t.is_empty()) {
                    let chunk = if self.output_transcript_buf.is_empty() {
                        text.trim_start().to_string()
                    } else {
                        text
                    };
                    self.output_transcript_buf.push_str(&chunk);

                    if let Some(pos) = self.output_transcript_buf.rfind(|c| matches!(c, '.' | '?' | '!')) {
                        let sentence = self.output_transcript_buf[..=pos].trim().to_string();
                        self.output_transcript_buf = self.output_transcript_buf[pos + 1..].trim_start().to_string();
                        if !sentence.is_empty() {
                            events.push(RealtimeEvent::OutputTranscription(sentence));
                        }
                    }
                }
            }

            // ── Barge-in / interruption ──────────────────────────────
            if matches!(sc.interrupted, Some(true)) {
                debug!("[gemini-live] Server signalled interruption");
                // Discard any buffered partial transcript on interruption.
                self.input_transcript_buf.clear();
                self.output_transcript_buf.clear();
                // No separate event needed — the NativeMultimodalBackend
                // cancels TTS when it sees barge-in from the local VAD.
            }

            // ── Model audio/text turn ────────────────────────────────
            if let Some(model_turn) = sc.model_turn {
                for part in model_turn.parts {
                    // Drop reasoning/thought tokens — never expose to TTS.
                    if matches!(part.thought, Some(true)) {
                        continue;
                    }

                    // Native audio output (primary path).
                    if let Some(inline) = part.inline_data {
                        if inline.mime_type.starts_with("audio/pcm") {
                            match Self::base64_to_pcm(&inline.data) {
                                Ok(samples) if !samples.is_empty() => {
                                    events.push(RealtimeEvent::BotAudioChunk(samples));
                                }
                                Ok(_) => {} // empty chunk, skip
                                Err(e) => {
                                    warn!("[gemini-live] PCM decode failed: {}", e);
                                }
                            }
                        }
                    }

                    // Text output (TEXT modality fallback, not audio).
                    if let Some(text) = part.text.filter(|t| !t.is_empty()) {
                        let chunk = if self.output_transcript_buf.is_empty() {
                            text.trim_start().to_string()
                        } else {
                            text
                        };
                        self.output_transcript_buf.push_str(&chunk);

                        if let Some(pos) = self.output_transcript_buf.rfind(|c| matches!(c, '.' | '?' | '!')) {
                            let sentence = self.output_transcript_buf[..=pos].trim().to_string();
                            self.output_transcript_buf = self.output_transcript_buf[pos + 1..].trim_start().to_string();
                            if !sentence.is_empty() {
                                events.push(RealtimeEvent::OutputTranscription(sentence));
                            }
                        }
                    }
                }
            }

            // ── Turn complete ────────────────────────────────────────
            if matches!(sc.turn_complete, Some(true)) {
                // Flush any buffered partial transcript that never hit punctuation
                // (e.g. a single-word utterance or a question without trailing `?`).
                let leftover_in = std::mem::take(&mut self.input_transcript_buf);
                let leftover_in = leftover_in.trim().to_string();
                if !leftover_in.is_empty() {
                    events.push(RealtimeEvent::InputTranscription(leftover_in));
                }

                let leftover_out = std::mem::take(&mut self.output_transcript_buf);
                let leftover_out = leftover_out.trim().to_string();
                if !leftover_out.is_empty() {
                    events.push(RealtimeEvent::OutputTranscription(leftover_out));
                }

                let usage = self.pending_usage.take().unwrap_or_default();
                events.push(RealtimeEvent::TurnComplete {
                    prompt_tokens: usage.prompt_token_count.unwrap_or(0),
                    completion_tokens: usage.response_token_count.unwrap_or(0),
                });
            }
        }

        events
    }
}

#[async_trait]
impl RealtimeProvider for GeminiLiveProvider {
    /// Connect (or reconnect) to the Gemini Live WebSocket.
    ///
    /// If a `session_resumption_handle` is stored from a previous session,
    /// it is sent in the setup message to restore context transparently.
    async fn connect(&mut self, config: &RealtimeConfig) -> Result<(), LlmProviderError> {
        // Accept an externally-provided resumption handle.
        if let Some(ref ext_handle) = config.session_resumption_handle {
            self.session_resumption_handle = Some(ext_handle.clone());
        }

        let url = self.ws_url();
        info!(
            "[gemini-live] Connecting to {} (resumption={})",
            GEMINI_LIVE_WS_BASE,
            self.session_resumption_handle.is_some()
        );

        let (ws_stream, _response) = timeout(CONNECT_TIMEOUT, connect_async(&url))
            .await
            .map_err(|_| LlmProviderError::Transport("Connection timed out".to_string()))?
            .map_err(|e| LlmProviderError::Transport(format!("WebSocket connect: {e}")))?;

        self.ws = Some(ws_stream);

        // Send setup immediately after connecting.
        self.send_setup(config).await?;

        info!("[gemini-live] Connected and setup sent");
        Ok(())
    }

    fn session_resumption_handle(&self) -> Option<String> {
        self.session_resumption_handle.clone()
    }

    /// Stream raw PCM audio from the user's microphone into the Gemini backend.
    ///
    /// Converts `i16` samples → little-endian bytes → base64, then sends as
    /// `BidiGenerateContentRealtimeInput` with mime `audio/pcm;rate=16000`.
    async fn push_user_audio(&mut self, pcm_data: &[i16]) -> Result<(), LlmProviderError> {
        if pcm_data.is_empty() {
            return Ok(());
        }
        let ws = self.ws.as_mut().ok_or_else(|| {
            LlmProviderError::Transport("Not connected".to_string())
        })?;

        let msg = RealtimeInputMessage {
            realtime_input: RealtimeInputPayload {
                audio: Some(BlobPayload {
                    data: Self::pcm_to_base64(pcm_data),
                    mime_type: format!("audio/pcm;rate={INPUT_SAMPLE_RATE}"),
                }),
                ..Default::default()
            },
        };

        let json = serde_json::to_string(&msg)
            .map_err(|e| LlmProviderError::Provider(format!("audio serialize: {e}")))?;
        ws.send(Message::Text(json.into()))
            .await
            .map_err(|e| LlmProviderError::Transport(format!("audio send: {e}")))?;
        Ok(())
    }

    /// Signal user voice activity to the backend (client-side VAD mode).
    ///
    /// When a client-side VAD (e.g. Silero) detects speech start/stop, call
    /// this method instead of relying on Google's server-side VAD.
    async fn trigger_vad(&mut self, state: VadState) -> Result<(), LlmProviderError> {
        let ws = self.ws.as_mut().ok_or_else(|| {
            LlmProviderError::Transport("Not connected".to_string())
        })?;

        let payload = match state {
            VadState::Started => RealtimeInputPayload {
                activity_start: Some(Value::Object(Default::default())),
                ..Default::default()
            },
            VadState::Stopped => RealtimeInputPayload {
                activity_end: Some(Value::Object(Default::default())),
                ..Default::default()
            },
        };

        let msg = RealtimeInputMessage {
            realtime_input: payload,
        };

        let json = serde_json::to_string(&msg)
            .map_err(|e| LlmProviderError::Provider(format!("vad serialize: {e}")))?;
        ws.send(Message::Text(json.into()))
            .await
            .map_err(|e| LlmProviderError::Transport(format!("vad trigger send: {e}")))?;
        Ok(())
    }

    /// Wait for the next event from Gemini.
    ///
    /// This method blocks until a meaningful event arrives (audio, tool call,
    /// transcription, or turn completion). Internally skips over messages whose
    /// content is irrelevant (usage-only, empty server content, etc.).
    async fn recv_event(&mut self) -> Result<RealtimeEvent, LlmProviderError> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(event);
            }

            let msg = match self.next_server_message().await {
                Ok(m) => {
                    m
                }
                Err(e) => {
                    // A WebSocket error means the stream is dead — propagate immediately.
                    // Reconnect is handled by NativeMultimodalBackend (via session.rs).
                    warn!("[gemini-live] recv error: {}", e);
                    return Err(e);
                }
            };

            // Active start (Gemini 3.x):
            //
            // After setupComplete, send a single-space `realtimeInput` to kick off inference.
            // This is the minimal trigger used by pipecat for Gemini 3.x models.
            // `clientContent` causes "Invalid argument" when concurrent mic audio is
            // already streaming into the socket.
            if msg.setup_complete.is_some() && self.session_resumption_handle.is_none() {
                info!("[gemini-live] Setup complete. Sending active-start trigger.");

                let realtime_trigger = RealtimeInputMessage {
                    realtime_input: RealtimeInputPayload {
                        text: Some(" ".to_string()),
                        ..Default::default()
                    },
                };

                if let Some(ws) = self.ws.as_mut() {
                    if let Ok(json) = serde_json::to_string(&realtime_trigger) {
                        let _ = ws.send(Message::Text(json.into())).await;
                    }
                }
            }

            let events = self.parse_server_message(msg);
            for e in events {
                self.pending_events.push_back(e);
            }
        }
    }

    /// Send a tool result back to Gemini to continue the audio turn.
    ///
    /// After the `NativeMultimodalBackend` runs the tool and gets a result,
    /// it must call this method so the model can resume speaking.
    async fn send_tool_result(
        &mut self,
        call_id: &str,
        tool_name: &str,
        result: Value,
    ) -> Result<(), LlmProviderError> {
        let ws = self.ws.as_mut().ok_or_else(|| {
            LlmProviderError::Transport("Not connected".to_string())
        })?;

        let msg = ToolResponseMessage {
            tool_response: ToolResponsePayload {
                function_responses: vec![FunctionResponse {
                    id: call_id.to_string(),
                    name: tool_name.to_string(),
                    response: result,
                }],
            },
        };

        let json = serde_json::to_string(&msg)
            .map_err(|e| LlmProviderError::Provider(format!("tool response serialize: {e}")))?;
        ws.send(Message::Text(json.into()))
            .await
            .map_err(|e| LlmProviderError::Transport(format!("tool response send: {e}")))?;
        Ok(())
    }

    /// Interrupt the current model response (barge-in / cut-off).
    ///
    /// Gemini Live interprets `activityStart` as an interruption signal — sending
    /// it causes the server to immediately stop its current audio output and begin
    /// listening for new user speech.
    async fn interrupt(&mut self) -> Result<(), LlmProviderError> {
        self.trigger_vad(VadState::Started).await
    }
}
