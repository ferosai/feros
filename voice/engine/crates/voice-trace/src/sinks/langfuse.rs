#[cfg(feature = "langfuse")]
use std::collections::HashSet;

#[cfg(feature = "langfuse")]
use opentelemetry::trace::TracerProvider as _;
#[cfg(feature = "langfuse")]
use opentelemetry::KeyValue;
#[cfg(feature = "langfuse")]
use opentelemetry_sdk::trace::SdkTracerProvider;
#[cfg(feature = "langfuse")]
use tracing::info;

#[cfg(feature = "langfuse")]
use std::sync::OnceLock;

#[cfg(feature = "langfuse")]
use crate::{to_sink_event, Event, EventCategory, SinkEvent, Tracer};

#[cfg(feature = "langfuse")]
static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
#[cfg(feature = "langfuse")]
static CONFIG: OnceLock<LangfuseConfig> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub public_key: String,
    pub secret_key: String,
    pub base_url: String,
    pub trace_public: bool,
}

impl Default for LangfuseConfig {
    fn default() -> Self {
        Self {
            public_key: String::new(),
            secret_key: String::new(),
            base_url: "https://cloud.langfuse.com".to_string(),
            trace_public: false,
        }
    }
}

impl LangfuseConfig {
    pub fn is_enabled(&self) -> bool {
        !self.public_key.is_empty() && !self.secret_key.is_empty()
    }
}

#[cfg(feature = "langfuse")]
enum ProcessorMsg {
    Span(opentelemetry_sdk::trace::SpanData),
    Flush,
}

#[cfg(feature = "langfuse")]
struct LangfuseProcessor {
    tx: std::sync::mpsc::Sender<ProcessorMsg>,
}

#[cfg(feature = "langfuse")]
impl LangfuseProcessor {
    fn new(exporter: opentelemetry_otlp::SpanExporter) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<ProcessorMsg>();

        std::thread::Builder::new()
            .name("langfuse-export".into())
            .spawn(move || {
                use opentelemetry_sdk::trace::SpanExporter as _;
                let exporter = exporter;
                let mut batch: Vec<opentelemetry_sdk::trace::SpanData> = Vec::with_capacity(64);
                let flush_interval = std::time::Duration::from_secs(5);

                loop {
                    match rx.recv_timeout(flush_interval) {
                        Ok(ProcessorMsg::Span(sd)) => {
                            batch.push(sd);
                            while let Ok(msg) = rx.try_recv() {
                                match msg {
                                    ProcessorMsg::Span(sd) => batch.push(sd),
                                    ProcessorMsg::Flush => break,
                                }
                            }
                        }
                        Ok(ProcessorMsg::Flush) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            if !batch.is_empty() {
                                let _ = futures_util::FutureExt::now_or_never(
                                    exporter.export(std::mem::take(&mut batch)),
                                );
                            }
                            return;
                        }
                    }

                    if !batch.is_empty() {
                        match futures_util::FutureExt::now_or_never(
                            exporter.export(std::mem::take(&mut batch)),
                        ) {
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                tracing::warn!("[langfuse] Export error: {:?}", e);
                            }
                            None => {
                                tracing::warn!("[langfuse] Export future not ready");
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn langfuse export thread");

        Self { tx }
    }
}

#[cfg(feature = "langfuse")]
impl std::fmt::Debug for LangfuseProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LangfuseProcessor").finish()
    }
}

#[cfg(feature = "langfuse")]
impl opentelemetry_sdk::trace::SpanProcessor for LangfuseProcessor {
    fn on_start(
        &self,
        _span: &mut opentelemetry_sdk::trace::Span,
        _cx: &opentelemetry::Context,
    ) {
    }

    fn on_end(&self, span: opentelemetry_sdk::trace::SpanData) {
        let _ = self.tx.send(ProcessorMsg::Span(span));
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        let _ = self.tx.send(ProcessorMsg::Flush);
        Ok(())
    }

    fn shutdown_with_timeout(
        &self,
        _timeout: std::time::Duration,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        let _ = self.tx.send(ProcessorMsg::Flush);
        Ok(())
    }
}

#[cfg(feature = "langfuse")]
pub fn init(config: &LangfuseConfig) {
    if !config.is_enabled() {
        info!("[langfuse] Tracing disabled (no keys configured)");
        return;
    }

    use base64::Engine as _;
    use opentelemetry_otlp::{SpanExporter, WithExportConfig, WithHttpConfig};

    let credentials = format!("{}:{}", config.public_key, config.secret_key);
    let auth = base64::engine::general_purpose::STANDARD.encode(credentials);
    let auth_header = format!("Basic {}", auth);

    let endpoint = format!("{}/api/public/otel/v1/traces", config.base_url);

    let mut headers = std::collections::HashMap::new();
    headers.insert("Authorization".to_string(), auth_header);

    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .with_headers(headers)
        .build()
        .expect("Failed to build Langfuse OTLP exporter");

    let processor = LangfuseProcessor::new(exporter);
    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .build();

    let _ = PROVIDER.set(provider);
    let _ = CONFIG.set(config.clone());

    info!("[langfuse] Tracer initialized → {}", endpoint);
}

#[cfg(feature = "langfuse")]
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get() {
        if let Err(e) = provider.shutdown() {
            tracing::warn!("[langfuse] Shutdown error: {:?}", e);
        }
    }
}

#[cfg(feature = "langfuse")]
pub struct LangfuseSessionObserver {
    session_id: String,
    trace_public: bool,
    otel_tracer: opentelemetry_sdk::trace::Tracer,
    conversation_cx: opentelemetry::Context,
    current_turn_span: Option<opentelemetry_sdk::trace::Span>,
    current_turn_cx: Option<opentelemetry::Context>,
}

#[cfg(feature = "langfuse")]
impl LangfuseSessionObserver {
    pub fn new(session_id: String) -> Option<Self> {
        use opentelemetry::trace::{Span as _, TraceContextExt, Tracer as OtelTracer};

        let provider = PROVIDER.get()?;
        let trace_public = CONFIG.get().map(|c| c.trace_public).unwrap_or(false);
        let otel_tracer = provider.tracer("voice-engine");

        let mut root_span = otel_tracer.start("conversation");
        root_span.set_attribute(KeyValue::new("conversation.id", session_id.clone()));
        root_span.set_attribute(KeyValue::new("conversation.type", "voice"));
        if trace_public {
            root_span.set_attribute(KeyValue::new("langfuse.trace.public", true));
        }
        let conversation_cx = opentelemetry::Context::current().with_span(root_span);

        Some(Self {
            session_id,
            trace_public,
            otel_tracer,
            conversation_cx,
            current_turn_span: None,
            current_turn_cx: None,
        })
    }

    pub fn on_event(&mut self, event: &Event) {
        if let Some(db_ev) = to_sink_event(event) {
            self.on_sink_event(&db_ev);
        }
    }

    pub fn on_sink_event(&mut self, event: &SinkEvent) {
        use opentelemetry::trace::{Span as _, TraceContextExt, Tracer as OtelTracer};

        match event {
            SinkEvent::TurnStarted { turn_number } => {
                if let Some(mut span) = self.current_turn_span.take() {
                    span.set_attribute(KeyValue::new("turn.was_interrupted", false));
                    span.end();
                }

                let mut span = self
                    .otel_tracer
                    .span_builder(format!("turn-{}", turn_number))
                    .start_with_context(&self.otel_tracer, &self.conversation_cx);
                span.set_attribute(KeyValue::new("turn.number", *turn_number as i64));
                span.set_attribute(KeyValue::new("turn.type", "conversation"));
                span.set_attribute(KeyValue::new("conversation.id", self.session_id.clone()));
                if self.trace_public {
                    span.set_attribute(KeyValue::new("langfuse.trace.public", true));
                }

                let span_cx = span.span_context().clone();
                let turn_cx = self.conversation_cx.with_remote_span_context(span_cx);
                self.current_turn_cx = Some(turn_cx);
                self.current_turn_span = Some(span);
            }
            SinkEvent::TurnEnded {
                turn_number,
                was_interrupted,
                turn_duration_ms,
                user_agent_latency_ms,
                vad_silence_ms,
            } => {
                if let Some(mut span) = self.current_turn_span.take() {
                    span.set_attribute(KeyValue::new("turn.number", *turn_number as i64));
                    span.set_attribute(KeyValue::new("turn.was_interrupted", *was_interrupted));
                    if let Some(dur) = turn_duration_ms {
                        span.set_attribute(KeyValue::new("turn.duration_ms", *dur));
                    }
                    if let Some(lat) = user_agent_latency_ms {
                        span.set_attribute(KeyValue::new("turn.user_agent_latency_ms", *lat));
                    }
                    if let Some(vad) = vad_silence_ms {
                        span.set_attribute(KeyValue::new("turn.vad_silence_ms", *vad));
                    }
                    span.end();
                }
                self.current_turn_cx = None;
            }
            SinkEvent::SttComplete {
                provider,
                model,
                transcript,
                is_final,
                language,
                duration_ms,
                ttfb_ms,
                vad_enabled,
            } => {
                self.with_historical_span("stt", *duration_ms, |span| {
                    span.set_attribute(KeyValue::new("gen_ai.system", provider.clone()));
                    span.set_attribute(KeyValue::new("gen_ai.request.model", model.clone()));
                    span.set_attribute(KeyValue::new("gen_ai.operation.name", "stt"));
                    span.set_attribute(KeyValue::new("output", transcript.clone()));
                    span.set_attribute(KeyValue::new("is_final", *is_final));
                    span.set_attribute(KeyValue::new("vad_enabled", *vad_enabled));
                    if let Some(lang) = language {
                        span.set_attribute(KeyValue::new("language", lang.clone()));
                    }
                    span.set_attribute(KeyValue::new("metrics.duration_ms", *duration_ms));
                    if let Some(ttfb) = ttfb_ms {
                        span.set_attribute(KeyValue::new("metrics.ttfb_ms", *ttfb));
                    }
                });
            }
            SinkEvent::LlmComplete {
                provider,
                model,
                input_json,
                output_json,
                tools_json,
                temperature,
                max_tokens,
                duration_ms,
                ttfb_ms,
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                span_label,
            } => {
                self.with_historical_span(span_label, *duration_ms, |span| {
                    span.set_attribute(KeyValue::new("gen_ai.system", provider.clone()));
                    span.set_attribute(KeyValue::new("gen_ai.request.model", model.clone()));
                    span.set_attribute(KeyValue::new("gen_ai.operation.name", "chat"));
                    span.set_attribute(KeyValue::new("gen_ai.output.type", "text"));
                    span.set_attribute(KeyValue::new("gen_ai.request.temperature", *temperature));
                    span.set_attribute(KeyValue::new("gen_ai.request.max_tokens", *max_tokens as i64));
                    span.set_attribute(KeyValue::new("stream", true));

                    let input = if let Some(tools) = tools_json {
                        format!(r#"{{"messages":{},"tools":{}}}"#, input_json, tools)
                    } else {
                        format!(r#"{{"messages":{}}}"#, input_json)
                    };
                    span.set_attribute(KeyValue::new("input", input));
                    span.set_attribute(KeyValue::new("output", output_json.clone()));

                    span.set_attribute(KeyValue::new("gen_ai.usage.input_tokens", *prompt_tokens as i64));
                    span.set_attribute(KeyValue::new(
                        "gen_ai.usage.output_tokens",
                        *completion_tokens as i64,
                    ));
                    if let Some(cached) = cache_read_tokens {
                        span.set_attribute(KeyValue::new(
                            "gen_ai.usage.cache_read_input_tokens",
                            *cached as i64,
                        ));
                    }

                    span.set_attribute(KeyValue::new("metrics.duration_ms", *duration_ms));
                    if let Some(ttfb) = ttfb_ms {
                        span.set_attribute(KeyValue::new("metrics.ttfb_ms", *ttfb));
                    }
                });
            }
            SinkEvent::TtsComplete {
                provider,
                model,
                text,
                voice_id,
                character_count,
                duration_ms,
                ttfb_ms,
                text_aggregation_ms,
            } => {
                self.with_historical_span("tts", *duration_ms, |span| {
                    span.set_attribute(KeyValue::new("gen_ai.system", provider.clone()));
                    span.set_attribute(KeyValue::new("gen_ai.request.model", model.clone()));
                    span.set_attribute(KeyValue::new("gen_ai.operation.name", "tts"));
                    span.set_attribute(KeyValue::new("gen_ai.output.type", "speech"));
                    span.set_attribute(KeyValue::new("input", text.clone()));
                    span.set_attribute(KeyValue::new("voice_id", voice_id.clone()));
                    span.set_attribute(KeyValue::new(
                        "metrics.character_count",
                        *character_count as i64,
                    ));
                    span.set_attribute(KeyValue::new("metrics.duration_ms", *duration_ms));
                    if let Some(ttfb) = ttfb_ms {
                        span.set_attribute(KeyValue::new("metrics.ttfb_ms", *ttfb));
                    }
                    if let Some(agg) = text_aggregation_ms {
                        span.set_attribute(KeyValue::new("metrics.text_aggregation_ms", *agg));
                    }
                });
            }
            _ => {}
        }
    }

    pub fn finish(mut self) {
        use opentelemetry::trace::{Span as _, TraceContextExt};
        if let Some(mut span) = self.current_turn_span.take() {
            span.end();
        }
        self.conversation_cx.span().end();
        info!("[langfuse] Session subscriber stopped: {}", self.session_id);
    }

    fn with_historical_span<F>(&self, span_name: &str, duration_ms: f64, mut f: F)
    where
        F: FnMut(&mut opentelemetry_sdk::trace::Span),
    {
        use opentelemetry::trace::{Span as _, Tracer as OtelTracer};

        let parent = self.current_turn_cx.as_ref().unwrap_or(&self.conversation_cx);

        // Approximate: end_time is the observer-receipt time, not the true
        // completion instant. Any event-queue delay shifts both ends equally,
        // so the span duration is exactly accurate but absolute placement may be
        // off by a few milliseconds relative to actual execution.
        let end_time = std::time::SystemTime::now();
        let start_time = end_time
            .checked_sub(std::time::Duration::from_secs_f64(duration_ms.max(0.0) / 1000.0))
            .unwrap_or(end_time);

        let mut span = self
            .otel_tracer
            .span_builder(span_name.to_string())
            .with_start_time(start_time)
            .start_with_context(&self.otel_tracer, parent);

        f(&mut span);

        span.set_attribute(KeyValue::new("conversation.id", self.session_id.clone()));
        if self.trace_public {
            span.set_attribute(KeyValue::new("langfuse.trace.public", true));
        }
        span.end_with_timestamp(end_time);
    }
}

#[cfg(feature = "langfuse")]
pub fn spawn_langfuse_subscriber(tracer: &Tracer, session_id: String) {
    let Some(provider) = PROVIDER.get() else {
        return;
    };
    let _ = provider;

    let mut rx = tracer.subscribe_filtered(HashSet::from([EventCategory::Observability]));

    tokio::spawn(async move {
        let Some(mut observer) = LangfuseSessionObserver::new(session_id) else {
            return;
        };
        while let Some(event) = rx.recv().await {
            observer.on_event(&event);
        }
        observer.finish();
    });
}

#[cfg(not(feature = "langfuse"))]
pub fn init(_config: &LangfuseConfig) {}

#[cfg(not(feature = "langfuse"))]
pub fn shutdown() {}

#[cfg(not(feature = "langfuse"))]
pub struct LangfuseSessionObserver;

#[cfg(not(feature = "langfuse"))]
impl LangfuseSessionObserver {
    pub fn new(_session_id: String) -> Option<Self> {
        None
    }
    pub fn on_event(&mut self, _event: &crate::Event) {}
    pub fn on_sink_event(&mut self, _event: &crate::SinkEvent) {}
    pub fn finish(self) {}
}

#[cfg(not(feature = "langfuse"))]
pub fn spawn_langfuse_subscriber(_tracer: &crate::Tracer, _session_id: String) {}
