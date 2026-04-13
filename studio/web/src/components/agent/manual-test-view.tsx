"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import { Redo03Icon, ArrowTurnBackwardIcon, CallDisabled02Icon, Cancel01Icon, CheckmarkCircle02Icon, FlashIcon, Mic01Icon, PlayIcon, MessageMultiple01Icon } from "@hugeicons/core-free-icons";
import { type ReactNode, useState, useRef, useEffect, useCallback } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { type Agent, api, WS_BASE } from "@/lib/api/client";

// ── Types ────────────────────────────────────────────────────────

type TimelineEntry =
  | { kind: "message"; role: string; text: string;
      ttft_ms?: number | null; first_sentence_ms?: number | null;
      total_turn_ms?: number; tokens_per_second?: number }
  | { kind: "tool"; tool_call_id?: string; tool_name: string;
      status: "executing" | "completed" | "error" | "interrupted" | "orphaned";
      duration_ms?: number; timestamp: number; error_message?: string };

type ToolTimelineEntry = Extract<TimelineEntry, { kind: "tool" }>;
type ToolStatus = ToolTimelineEntry["status"];

interface ToolActivityPayload {
  tool_call_id?: string;
  tool_name: string;
  status: ToolStatus;
  error_message?: string;
}

type VoiceState = "idle" | "connecting" | "listening" | "processing" | "speaking" | "toolcalling";

const AUTO_SCROLL_THRESHOLD_PX = 96;

/** Sum duration_ms of tool entries immediately preceding index `i` in the timeline. */
function _toolDurationBefore(timeline: TimelineEntry[], i: number): number | null {
  let total = 0;
  for (let j = i - 1; j >= 0; j--) {
    const e = timeline[j];
    if (e.kind === "tool") { total += e.duration_ms ?? 0; }
    else break; // stop at the first non-tool entry
  }
  return total > 0 ? total : null;
}

function parseToolActivityPayload(payload: Record<string, unknown>): ToolActivityPayload | null {
  const toolName = typeof payload.tool_name === "string" ? payload.tool_name : "";
  const status = payload.status;
  if (
    !toolName
    || status !== "executing"
    && status !== "completed"
    && status !== "error"
    && status !== "interrupted"
    && status !== "orphaned"
  ) {
    return null;
  }

  return {
    tool_call_id: typeof payload.tool_call_id === "string" ? payload.tool_call_id : undefined,
    tool_name: toolName,
    status,
    error_message:
      typeof payload.error_message === "string" && payload.error_message
        ? payload.error_message
        : undefined,
  };
}

function applyToolActivity(
  timeline: TimelineEntry[],
  activity: ToolActivityPayload,
): TimelineEntry[] {
  if (activity.status === "executing") {
    return [
      ...timeline,
      {
        kind: "tool",
        tool_call_id: activity.tool_call_id,
        tool_name: activity.tool_name,
        status: "executing",
        timestamp: Date.now(),
      },
    ];
  }

  const idx = [...timeline].reverse().findIndex((entry) =>
    entry.kind === "tool"
    && entry.status === "executing"
    && (
      activity.tool_call_id && entry.tool_call_id
        ? entry.tool_call_id === activity.tool_call_id
        : entry.tool_name === activity.tool_name
    ));

  if (idx < 0) {
    return [
      ...timeline,
      {
        kind: "tool",
        tool_call_id: activity.tool_call_id,
        tool_name: activity.tool_name,
        status: activity.status,
        error_message: activity.error_message,
        timestamp: Date.now(),
      },
    ];
  }

  const realIdx = timeline.length - 1 - idx;
  const next = [...timeline];
  const old = next[realIdx] as ToolTimelineEntry;
  next[realIdx] = {
    ...old,
    status: activity.status,
    duration_ms: Date.now() - old.timestamp,
    error_message: activity.error_message,
  };
  return next;
}

// ── Props ────────────────────────────────────────────────────────

interface ManualTestViewProps {
  agentId: string;
  agent: Agent | null;
  activeMode: "voice" | "text";
  onModeChange: (mode: "voice" | "text") => void;
  onGoToConfig?: () => void;
}

export default function ManualTestView({
  agentId,
  agent,
  activeMode,
  onModeChange: _onModeChange,
  onGoToConfig,
}: ManualTestViewProps) {
  const id = agentId;


  const config = agent?.current_config;
  const isMissingVoiceId = agent !== null && !config?.voice_id?.trim();

  // Voice test state
  const [voiceState, setVoiceState] = useState<VoiceState>("idle");
  const [voiceTimeline, setVoiceTimeline] = useState<TimelineEntry[]>([]);
  // WebRTC refs
  const pcRef = useRef<RTCPeerConnection | null>(null);
  const remoteAudioRef = useRef<HTMLAudioElement | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  // WebSocket ref (for UI events — transcripts, state, tools)
  const wsRef = useRef<WebSocket | null>(null);
  // Voice server URL — from Settings, defaults to localhost:8300 if not configured
  const [voiceServerUrl, setVoiceServerUrl] = useState("http://localhost:8300");
  const attemptedVoiceServerUrlRef = useRef("http://localhost:8300");
  // Voice error dedup: at most one toast per session, suppress on user-initiated stop
  const voiceStopExpectedRef = useRef(false);
  const voiceToastShownRef = useRef(false);

  const showVoiceErrorToast = useCallback((errorMsg?: string) => {
    if (voiceToastShownRef.current || voiceStopExpectedRef.current) return;
    voiceToastShownRef.current = true;
    const currentVoiceServerUrl = attemptedVoiceServerUrlRef.current || voiceServerUrl || "unknown";
    toast.error(`Voice server error (${currentVoiceServerUrl})`, {
      description: errorMsg || "Voice test could not continue because the voice server had an error.",
      duration: 8000,
    });
  }, [voiceServerUrl]);

  useEffect(() => {
    api.settings.getTelephony().then((s) => {
      if (s.voice_server_url) {
        const normalizedVoiceServerUrl = s.voice_server_url.replace(/\/$/, "");
        setVoiceServerUrl(normalizedVoiceServerUrl);
        attemptedVoiceServerUrlRef.current = normalizedVoiceServerUrl;
      }
    }).catch(() => { /* keep localhost default */ });
  }, []);

  // Text-only test state
  const textTestWsRef = useRef<WebSocket | null>(null);
  const [textTestConnected, setTextTestConnected] = useState(false);
  const [textTestConnecting, setTextTestConnecting] = useState(false);
  const [textTestProcessing, setTextTestProcessing] = useState(false);
  const [textTestTimeline, setTextTestTimeline] = useState<TimelineEntry[]>([]);
  const [textTestMsg, setTextTestMsg] = useState("");
  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const isNearBottomRef = useRef(true);

  const voiceStateMap: Record<VoiceState, { color: string; label: string; icon: ReactNode }> = {
    idle: { color: "bg-muted-foreground", label: "Ready", icon: <HugeiconsIcon icon={PlayIcon} className="size-5" /> },
    connecting: { color: "bg-muted-foreground animate-pulse", label: "Connecting...", icon: <Spinner className="size-5" /> },
    listening: { color: "bg-primary animate-pulse", label: "Listening", icon: <HugeiconsIcon icon={Mic01Icon} className="size-5 text-primary-foreground" /> },
    processing: { color: "bg-primary animate-pulse", label: "Thinking...", icon: <Spinner className="size-5 text-primary-foreground" /> },
    speaking: { color: "bg-orange-500 animate-pulse", label: "Speaking", icon: <HugeiconsIcon icon={Mic01Icon} className="size-5 text-primary-foreground" /> },
    toolcalling: { color: "bg-primary animate-pulse", label: "Running tool...", icon: <HugeiconsIcon icon={FlashIcon} className="size-5 text-primary-foreground" /> },
  };

  const updateNearBottom = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const distanceToBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    isNearBottomRef.current = distanceToBottom <= AUTO_SCROLL_THRESHOLD_PX;
  }, []);

  const scrollToBottom = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    isNearBottomRef.current = true;
  }, []);

  // ── Text test ──────────────────────────────────────────────────

  const startTextTest = useCallback(() => {
    setTextTestConnecting(true);
    setTextTestTimeline([]);
    isNearBottomRef.current = true;

    let ws: WebSocket;
    try {
      ws = new WebSocket(`${WS_BASE}/api/voice/text-test/${id}`);
    } catch {
      setTextTestConnecting(false);
      return;
    }
    textTestWsRef.current = ws;

    ws.onopen = () => { setTextTestConnecting(false); setTextTestConnected(true); };
    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data as string);
        if (data.type === "ready") {
          setTextTestTimeline([{ kind: "message", role: "assistant", text: data.greeting as string }]);
        } else if (data.type === "transcript") {
          setTextTestTimeline((prev) => [...prev, { kind: "message", role: data.role as string, text: data.text as string }]);
        } else if (data.type === "tool_activity") {
          const activity = parseToolActivityPayload(data as Record<string, unknown>);
          if (activity) {
            setTextTestTimeline((prev) => applyToolActivity(prev, activity));
          }
        } else if (data.type === "processing") {
          setTextTestProcessing(true);
        } else if (data.type === "turn_complete") {
          setTextTestProcessing(false);
        } else if (data.type === "metrics") {
          setTextTestTimeline((prev) => {
            const idx = [...prev].reverse().findIndex((e) => e.kind === "message" && e.role === "assistant");
            if (idx < 0) return prev;
            const realIdx = prev.length - 1 - idx;
            const next = [...prev];
            const old = next[realIdx] as TimelineEntry & { kind: "message" };
            next[realIdx] = {
              ...old,
              ttft_ms: data.ttft_ms as number | null,
              first_sentence_ms: data.first_sentence_ms as number | null,
              total_turn_ms: data.total_turn_ms as number,
              tokens_per_second: data.tokens_per_second as number,
            };
            return next;
          });
        } else if (data.type === "error") {
          setTextTestTimeline((prev) => [...prev, { kind: "message", role: "system", text: `Error: ${data.message as string}` }]);
          setTextTestProcessing(false);
        }
      } catch { /* ignore */ }
    };
    ws.onclose = () => { setTextTestConnected(false); setTextTestConnecting(false); setTextTestProcessing(false); textTestWsRef.current = null; };
    ws.onerror = () => { setTextTestConnected(false); setTextTestConnecting(false); setTextTestProcessing(false); };
  }, [id]);

  const stopTextTest = useCallback(() => {
    textTestWsRef.current?.close();
    textTestWsRef.current = null;
    setTextTestConnected(false);
  }, []);

  const sendTextMsg = useCallback(() => {
    const text = textTestMsg.trim();
    if (!text || !textTestWsRef.current || textTestWsRef.current.readyState !== WebSocket.OPEN) return;
    textTestWsRef.current.send(JSON.stringify({ type: "text", text }));
    setTextTestMsg("");
  }, [textTestMsg]);

  const handleTextMsgKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); sendTextMsg(); }
    },
    [sendTextMsg],
  );

  // ── Voice test (WebSocket for UI + WebRTC for audio) ────────────

  const stopVoice = useCallback(() => {
    // Null refs first to prevent re-entry from onclose/onerror callbacks
    const ws = wsRef.current; wsRef.current = null;
    const pc = pcRef.current; pcRef.current = null;
    const audio = remoteAudioRef.current;
    const stream = streamRef.current; streamRef.current = null;

    // Send explicit session.end before closing — this triggers the server's
    // clean shutdown path (TransportCommand::Close → reactor teardown) immediately,
    // rather than relying solely on the WS close event which may arrive late.
    if (ws && ws.readyState === WebSocket.OPEN) {
      try { ws.send(JSON.stringify({ type: "session.end" })); } catch { /* ignore */ }
    }
    ws?.close();
    pc?.close();
    if (audio) { audio.pause(); audio.srcObject = null; }
    stream?.getTracks().forEach((t) => t.stop());
    setVoiceState("idle");
  }, []);

  /** Handle WebSocket messages for UI updates (no audio) */
  const handleWsMsg = useCallback((ev: MessageEvent) => {
    if (ev.data instanceof ArrayBuffer || ev.data instanceof Blob) return; // ignore audio
    try {
      const msg = JSON.parse(ev.data);
      if (msg.type === "state_changed") {
        setVoiceState(msg.state);
      } else if (msg.type === "interrupt") {
        // No local audio buffer to flush — WebRTC audio is handled by the browser
      } else if (msg.type === "transcript") {
        setVoiceTimeline((p) => [...p, { kind: "message", role: msg.role, text: msg.text }]);
      } else if (msg.type === "tool_activity") {
        const activity = parseToolActivityPayload(msg as Record<string, unknown>);
        if (activity) {
          setVoiceTimeline((p) => applyToolActivity(p, activity));
        }
      } else if (msg.type === "session_ended") {
        // Normal agent hang-up — WS will close cleanly, not an error
        voiceStopExpectedRef.current = true;
      } else if (msg.type === "error") {
        showVoiceErrorToast(msg.message);
      }
    } catch {/* ignore */}
  }, [showVoiceErrorToast]);

  const startVoice = useCallback(async () => {
    try {
      voiceStopExpectedRef.current = false;
      voiceToastShownRef.current = false;
      setVoiceState("connecting");
      setVoiceTimeline([]);
      isNearBottomRef.current = true;

      let latestVoiceServerUrl = process.env.NEXT_PUBLIC_VOICE_SERVER_URL?.replace(/\/$/, "") || voiceServerUrl.replace(/\/$/, "");
      try {
        const s = await api.settings.getTelephony();
        if (s.voice_server_url) {
          latestVoiceServerUrl = s.voice_server_url.replace(/\/$/, "");
          setVoiceServerUrl(latestVoiceServerUrl); // update state so error toasts use the fresh one
        }
      } catch { /* keep defaults */ }
      attemptedVoiceServerUrlRef.current = latestVoiceServerUrl;

      if (!latestVoiceServerUrl) {
        stopVoice();
        toast.error("Voice server not configured", { description: "Set the Voice Server URL in Settings → Voice Infrastructure.", duration: 8000 });
        return;
      }

      // Attempt immediate unlock of the audio context within the click handler's transient activation
      if (remoteAudioRef.current) {
        remoteAudioRef.current.play().catch(() => {});
      }

      // 1. Get microphone — catch separately: mic errors are local, not server errors
      let stream: MediaStream;
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true, sampleRate: 48000 },
        });
      } catch {
        stopVoice();
        toast.error("Microphone unavailable", { description: "Allow microphone access and try again.", duration: 8000 });
        return;
      }
      streamRef.current = stream;

      // 2. Create session directly on voice-server (no Python needed)
      const voiceBase = latestVoiceServerUrl;
      const res = await fetch(`${voiceBase}/voice/session/${id}`, { method: "POST" });
      if (!res.ok) throw new Error(`Session registration failed: ${res.status}`);
      const { session_id, ws_url, token } = await res.json() as { session_id: string; ws_url: string; token: string };

      // 3. Defer WS attach until WebRTC offer/answer is established.
      // If WS connects first, the server can consume the pre-registered
      // session as a plain WS session and RTC offer will fail.

      // 4. Derive HTTP base from voice server for RTC offer
      const rtcBase = voiceBase.replace(/^wss?:\/\//, "https://").replace(/^ws:\/\//, "http://");

      // 5. Fetch ICE server config from voice-server
      let iceServers: RTCIceServer[] = [{ urls: "stun:stun.l.google.com:19302" }];
      try {
        const iceRes = await fetch(`${rtcBase}/rtc/ice-servers?session_id=${session_id}&token=${token}`);
        if (iceRes.ok) {
          const iceData = await iceRes.json() as { iceServers?: RTCIceServer[] };
          if (iceData.iceServers?.length) iceServers = iceData.iceServers;
        }
      } catch { /* fall back to Google STUN */ }

      // 6. Create RTCPeerConnection with real ICE servers
      const pc = new RTCPeerConnection({ iceServers });
      pcRef.current = pc;

      // 7. Explicitly add a bidirectional audio transceiver, then attach the primary mic track.
      // addTrack will reuse the transceiver's sender (per WebRTC spec §4.2.2).
      // We only add the first track to prevent multiple transceivers if the stream has multiple.
      pc.addTransceiver("audio", { direction: "sendrecv" });
      const firstTrack = stream.getAudioTracks()[0];
      if (firstTrack) pc.addTrack(firstTrack, stream);

      // 8. Handle incoming audio track from server
      pc.ontrack = (e) => {
        if (e.track.kind === "audio" && remoteAudioRef.current) {
          const audio = remoteAudioRef.current;
          audio.srcObject = e.streams && e.streams.length > 0 ? e.streams[0] : new MediaStream([e.track]);
          audio.play().catch((err) => {
            if (process.env.NODE_ENV === "development") console.warn("[voice] audio.play() returned an error:", err);
          });
        }
      };

      // Handle Trickle ICE candidates.
      // Candidates are buffered until after the offer POST resolves and
      // setRemoteDescription is called, because the server only inserts the
      // session into hybrid_sessions at the end of rtc_create_session.
      // Sending candidates before that window closes causes a guaranteed 404.
      const iceCandidateBuffer: string[] = [];
      let offerComplete = false;

      const sendIceCandidate = (candidateSdp: string) => {
        fetch(`${rtcBase}/rtc/candidate/${session_id}?token=${token}`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ candidate: candidateSdp }),
        })
          .then((res) => {
            if (!res.ok && process.env.NODE_ENV === "development") {
              console.warn(`[webrtc] ICE candidate POST returned ${res.status}`);
            }
          })
          .catch((err) => {
            if (process.env.NODE_ENV === "development") console.warn("[webrtc] Failed to send trickle ICE candidate", err);
          });
      };

      pc.onicecandidate = (event) => {
        if (event.candidate && event.candidate.candidate) {
          if (offerComplete) {
            sendIceCandidate(event.candidate.candidate);
          } else {
            iceCandidateBuffer.push(event.candidate.candidate);
          }
        }
      };

      pc.oniceconnectionstatechange = () => {
        if (pc.iceConnectionState === "failed") {
          stopVoice();
        }
      };


      // 9. Create SDP offer
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);

      // 10. Send SDP offer to voice-server immediately (Trickle ICE enabled)
      const rtcUrl = `${rtcBase}/rtc/offer/${session_id}?token=${token}`;
      const rtcRes = await fetch(rtcUrl, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ offer }),
      });
      
      const rtcData = await rtcRes.json();
      if (!rtcRes.ok) {
        throw new Error(rtcData.error || `WebRTC offer failed: ${rtcRes.status}`);
      }
      const { answer } = rtcData as { answer: RTCSessionDescriptionInit };

      // 12. Set remote description
      await pc.setRemoteDescription(new RTCSessionDescription(answer));

      // 13. Drain buffered ICE candidates now that the server session is live
      //     and setRemoteDescription has been applied locally.
      offerComplete = true;
      for (const c of iceCandidateBuffer) {
        sendIceCandidate(c);
      }
      iceCandidateBuffer.length = 0;

      // 14. Connect WebSocket for UI events (transcripts, state, tools)
      const ws = new WebSocket(ws_url);
      wsRef.current = ws;
      ws.onmessage = handleWsMsg;
      ws.onerror = () => {
        if (wsRef.current !== ws) return;
        showVoiceErrorToast();
        stopVoice();
      };
      ws.onclose = (ev) => {
        if (wsRef.current !== ws) return;
        if (ev.code === 1000 || ev.code === 1005) {
          voiceStopExpectedRef.current = true;
        } else {
          showVoiceErrorToast();
        }
        stopVoice();
      };

      setVoiceState("listening");
    } catch (err: unknown) {
      if (err instanceof Error && err.message) {
        toast.error("Test connection failed", { description: err.message, duration: 8000 });
      } else {
        showVoiceErrorToast();
      }
      stopVoice();
    }
  }, [id, handleWsMsg, stopVoice, showVoiceErrorToast, voiceServerUrl]);

  useEffect(() => () => { voiceStopExpectedRef.current = true; stopVoice(); }, [stopVoice]);

  useEffect(() => {
    updateNearBottom();
  }, [activeMode, updateNearBottom]);

  useEffect(() => {
    if (activeMode !== "text" || !isNearBottomRef.current) return;
    scrollToBottom();
    // activeMode is intentionally omitted so mode switches do not auto-scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [textTestTimeline, textTestProcessing, scrollToBottom]);

  useEffect(() => {
    if (activeMode !== "voice" || !isNearBottomRef.current) return;
    scrollToBottom();
    // activeMode is intentionally omitted so mode switches do not auto-scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [voiceTimeline, scrollToBottom]);

  // ── Render ─────────────────────────────────────────────────────
  const hasTextHistory = textTestTimeline.length > 0;
  const hasVoiceHistory = voiceTimeline.length > 0;
  const showTextBottomBar = activeMode === "text" && textTestConnected;
  const showTextRestartBar = activeMode === "text" && !textTestConnected && hasTextHistory;
  const showVoiceBottomBar = activeMode === "voice" && voiceState !== "idle";
  const showVoiceRestartBar =
    activeMode === "voice" && voiceState === "idle" && hasVoiceHistory;
  const showBottomBar =
    showTextBottomBar || showTextRestartBar || showVoiceBottomBar || showVoiceRestartBar;

  return (
    <div className="flex h-full min-h-0 flex-col bg-background relative overflow-hidden">
      {/* Audio element statically mounted to satisfy autoplay policy constraints reliably */}
      <audio ref={remoteAudioRef} autoPlay style={{ display: "none" }} />
      {/* Dynamic Background Accents */}
      <div className="absolute top-[-10%] right-[-10%] w-[40%] h-[40%] bg-primary/5 rounded-full blur-[120px] pointer-events-none" />
      <div className="absolute bottom-[-10%] left-[-10%] w-[30%] h-[30%] bg-primary/5 rounded-full blur-[100px] pointer-events-none" />


      <div
        ref={scrollContainerRef}
        onScroll={updateNearBottom}
        className="flex-1 min-h-0 overflow-y-auto custom-scrollbar relative z-10"
      >
        <div className="max-w-4xl mx-auto px-6 py-4 space-y-4">
          {/* Main Test Content */}
          {activeMode === "text" ? (
            <div className="space-y-4">
              {hasTextHistory ? (
                <>
                  <div className="space-y-4">
                    {textTestTimeline.map((entry, i) => (
                      <TimelineEntryCard key={i} entry={entry} previousEntries={textTestTimeline} index={i} />
                    ))}
                  </div>
                  {textTestProcessing && (
                    <div className="flex items-center gap-3 text-[10px] text-muted-foreground ml-3 animate-in fade-in slide-in-from-left-2 duration-300">
                      <div className="flex gap-1">
                        <span className="size-1 bg-primary rounded-full animate-bounce [animation-delay:-0.3s]" />
                        <span className="size-1 bg-primary rounded-full animate-bounce [animation-delay:-0.15s]" />
                        <span className="size-1 bg-primary rounded-full animate-bounce" />
                      </div>
                      <span className="font-medium">Thinking...</span>
                    </div>
                  )}
                </>
              ) : (
                <div className="flex flex-col items-center justify-center py-24 text-center">
                  <div className="size-32 rounded-full bg-background border-2 border-border shadow-sm flex items-center justify-center mb-8">
                    <HugeiconsIcon icon={MessageMultiple01Icon} className="size-10 text-muted-foreground/20" />
                  </div>
                  <h3 className="text-xl font-bold tracking-tight mb-2">Text Sandbox</h3>
                  <p className="max-w-[280px] text-xs text-muted-foreground leading-relaxed mb-8">
                    Interact with your agent via text to debug prompts and tool execution logic in real-time.
                  </p>
                  {!textTestConnected && (
                    <Button
                      onClick={startTextTest}
                      size="lg"
                      className="rounded-xl shadow-lg shadow-primary/10 gap-2 px-8 h-12 transition-all hover:-translate-y-px active:translate-y-0 ring-offset-background"
                      disabled={!agent?.current_config || textTestConnecting}
                    >
                      {textTestConnecting ? <Spinner className="size-4" /> : <HugeiconsIcon icon={PlayIcon} className="size-4 fill-current" />}
                      <span className="text-sm font-semibold">{textTestConnecting ? "Connecting..." : "Start Session"}</span>
                    </Button>
                  )}
                </div>
              )}
            </div>
          ) : (
            <div className="flex flex-col items-center">
              {hasVoiceHistory ? (
                <div className="w-full space-y-4">
                  {voiceTimeline.map((entry, i) => (
                    <TimelineEntryCard key={i} entry={entry} previousEntries={voiceTimeline} index={i} />
                  ))}
                </div>
              ) : (
                <div className="flex flex-col items-center justify-center py-24 text-center w-full">
                  {/* The "Vibe Sphere" */}
                  <div className="relative mb-8">
                    <div className={`absolute inset-0 rounded-full blur-3xl opacity-20 transition-all duration-700 ${
                      voiceState === "connecting" ? "bg-muted-foreground animate-pulse" :
                      voiceState === "listening" ? "bg-primary animate-pulse" :
                      voiceState === "processing" ? "bg-primary/50" :
                      voiceState === "speaking" ? "bg-orange-400 scale-110" :
                      "bg-primary/10"
                    }`} />

                    <div className={`relative size-32 rounded-full border-2 flex items-center justify-center transition-all duration-500 ${
                      voiceState === "idle" ? "bg-background border-border shadow-sm" :
                      "bg-background/80 backdrop-blur-sm border-primary/20 shadow-2xl"
                    }`}>
                      {voiceState === "idle" ? (
                        <HugeiconsIcon icon={Mic01Icon} className="size-10 text-muted-foreground/20" />
                      ) : (
                         <div className="relative flex items-center justify-center">
                            {voiceState === "speaking" && (
                              <div className="absolute inset-0 flex items-center justify-center">
                                <div className="size-24 rounded-full border-2 border-primary/10 animate-ping" />
                              </div>
                            )}
                            <div className={`transition-transform duration-300 ${voiceState === "speaking" ? "scale-110" : "scale-100"}`}>
                              {voiceStateMap[voiceState].icon}
                            </div>
                         </div>
                      )}
                    </div>
                  </div>

                  <h3 className="text-xl font-bold tracking-tight mb-2">
                    {voiceState === "idle" ? "Voice Sandbox" : voiceStateMap[voiceState].label}
                  </h3>
                  <p className="max-w-[320px] text-xs text-muted-foreground leading-relaxed mb-10">
                    {voiceState === "idle"
                      ? isMissingVoiceId
                        ? <span className="text-destructive font-semibold">No voice selected.{onGoToConfig && <> Go to <button onClick={onGoToConfig} className="underline underline-offset-2 hover:opacity-80">Voice Settings</button> to configure one.</>}</span>
                        : "Experience high-fidelity conversational AI with low-latency WebRTC streams."
                      : "Engaging in a real-time voice session. Speak naturally to interact."}
                  </p>

                  {voiceState === "idle" && (
                    <Button
                      onClick={startVoice}
                      size="lg"
                      className="rounded-xl shadow-lg shadow-primary/10 gap-2 px-8 h-12 transition-all hover:-translate-y-px active:translate-y-0 ring-offset-background"
                      disabled={!config || isMissingVoiceId}
                    >
                      <HugeiconsIcon icon={PlayIcon} className="size-4 fill-current" />
                      <span className="text-sm font-semibold">Start Session</span>
                    </Button>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Contextual Action Bar */}
      {showBottomBar && (
        <div className="z-30 p-4 bg-transparent pointer-events-none sticky bottom-0">
          <div className="max-w-2xl mx-auto w-full bg-card/95 backdrop-blur-xl border border-border/80 rounded-2xl shadow-premium pointer-events-auto flex items-center animate-in fade-in slide-in-from-bottom-4 duration-500 p-1.5">
            {showTextBottomBar && (
              <>
                <div className="flex-1 relative group pl-3 flex items-center min-h-[40px]">
                  <textarea
                    value={textTestMsg}
                    onChange={(e) => setTextTestMsg(e.target.value)}
                    onKeyDown={handleTextMsgKeyDown}
                    placeholder="Type to chat..."
                    rows={1}
                    disabled={textTestProcessing}
                    className="w-full resize-none bg-transparent py-2.5 text-sm leading-[20px] text-foreground outline-none border-none ring-0 focus:ring-0 transition-all placeholder:text-muted-foreground/40 max-h-[140px] disabled:opacity-50 block"
                    onInput={(e) => {
                      const el = e.currentTarget;
                      el.style.height = "40px";
                      const newHeight = Math.min(el.scrollHeight, 140);
                      el.style.height = `${newHeight}px`;
                    }}
                  />
                </div>
                <div className="flex items-center gap-1.5 pr-1.5">
                  <Button
                    size="icon"
                    onClick={sendTextMsg}
                    disabled={!textTestMsg.trim() || textTestProcessing}
                    className="rounded-xl h-10 w-10 shadow-lg shadow-primary/20 shrink-0 transition-all active:scale-95 active:shadow-inner"
                  >
                    {textTestProcessing ? <Spinner className="size-4" /> : <HugeiconsIcon icon={ArrowTurnBackwardIcon} className="size-4" />}
                  </Button>
                  <div className="w-px h-5 bg-border/60 mx-1" />
                  <Button
                    onClick={stopTextTest}
                    variant="ghost"
                    size="icon"
                    className="rounded-xl h-10 w-10 text-muted-foreground hover:text-destructive hover:bg-destructive/10 shrink-0 transition-all active:scale-95"
                    title="End Session"
                  >
                    <HugeiconsIcon icon={Cancel01Icon} className="size-5" />
                  </Button>
                </div>
              </>
            )}

            {showTextRestartBar && (
              <Button
                onClick={startTextTest}
                className="w-full rounded-xl gap-2.5 h-10 shadow-lg shadow-primary/10 transition-all active:scale-[0.98]"
                disabled={!agent?.current_config || textTestConnecting}
              >
                {textTestConnecting ? <Spinner className="size-4" /> : <HugeiconsIcon icon={Redo03Icon} className="size-4" />}
                <span className="text-sm font-bold">Restart Debug Session</span>
              </Button>
            )}

            {showVoiceBottomBar && (
              <div className="w-full flex items-center justify-between gap-4 px-1.5">
                <div className="flex items-center gap-3">
                   <div className="size-8 rounded-full bg-primary/10 flex items-center justify-center relative overflow-hidden group">
                      <div className="absolute inset-0 bg-primary/20 animate-pulse" />
                      <HugeiconsIcon icon={Mic01Icon} className="size-3.5 text-primary relative z-10" />
                   </div>
                   <div>
                     <div className="text-[10px] font-bold uppercase tracking-wider text-muted-foreground/60 leading-none mb-1">Status</div>
                     <div className="text-[10px] font-bold text-foreground leading-none">{voiceStateMap[voiceState].label}</div>
                   </div>
                </div>

                {/* Simulated Waveform Visualizer */}
                <div className="flex items-baseline gap-0.5 h-6 flex-1 justify-center max-w-[120px]">
                  {[1, 2, 3, 4, 5, 4, 3, 2, 1].map((h, i) => (
                    <div
                      key={i}
                      className={`w-0.5 bg-primary/30 rounded-full transition-all duration-300 ${voiceState === "speaking" ? "animate-wave" : "h-1"}`}
                      style={{
                        height: voiceState === "speaking" ? `${h * 15 + Math.random() * 20}%` : "4px",
                        animationDelay: `${i * 0.1}s`
                      }}
                    />
                  ))}
                </div>

                <Button
                  onClick={() => { voiceStopExpectedRef.current = true; stopVoice(); }}
                  variant="destructive"
                  className="rounded-xl px-3.5 h-10 gap-2 shadow-lg shadow-destructive/20 hover:scale-[1.02] active:scale-[0.98] transition-all"
                >
                  <HugeiconsIcon icon={CallDisabled02Icon} className="size-3.5" />
                  <span className="text-[10px] font-bold">End Session</span>
                </Button>
              </div>
            )}

            {showVoiceRestartBar && (
              <Button
                onClick={startVoice}
                className="w-full rounded-xl gap-2.5 h-10 shadow-lg shadow-primary/10 transition-all active:scale-[0.98]"
                disabled={!config || isMissingVoiceId}
              >
                <HugeiconsIcon icon={Mic01Icon} className="size-4" />
                <span className="text-sm font-bold">Restart Voice Session</span>
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Internal Components ──────────────────────────────────────────

function TimelineEntryCard({ entry, previousEntries: _previousEntries, index: _index }: {
  entry: TimelineEntry,
  previousEntries: TimelineEntry[],
  index: number
}) {
  if (entry.kind === "message") {
    const isUser = entry.role === "user";
    const isSystem = entry.role === "system";
    const name = isUser ? "You" : isSystem ? "System" : "Agent";

    return (
      <div className={`group animate-in fade-in slide-in-from-bottom-2 duration-400 fill-mode-both ${isUser ? "ml-12" : "mr-12"}`}>
        <div className="flex items-center gap-2 mb-1.5 px-1">
          <div className={`text-[10px] font-bold uppercase tracking-[0.15em] ${isUser ? "text-primary ml-auto" : isSystem ? "text-destructive" : "text-muted-foreground/80"}`}>
            {name}
          </div>
        </div>
        <div className={`relative px-4 py-3 rounded-2xl border text-sm leading-relaxed shadow-sm transition-shadow hover:shadow-md ${
          isUser
            ? "bg-primary text-primary-foreground border-primary/10 rounded-tr-none ml-auto w-fit max-w-[90%]"
            : isSystem
            ? "bg-destructive/5 border-destructive/20 text-destructive text-center w-full"
            : "bg-card/40 backdrop-blur-sm border-border/80 rounded-tl-none w-fit max-w-[90%]"
        }`}>
          {entry.text}

          {!isUser && !isSystem && entry.total_turn_ms != null && (
            <div className="mt-3 pt-3 border-t border-border/50 flex flex-wrap items-center gap-y-2 gap-x-4 opacity-70 transition-opacity group-hover:opacity-100">
               <div className="flex items-center gap-1.5">
                  <HugeiconsIcon icon={FlashIcon} className="size-2.5 text-primary" />
                  <span className="text-[10px] font-medium text-muted-foreground tabular-nums">
                    {entry.ttft_ms}ms <span className="text-[10px] opacity-60">TTFT</span>
                  </span>
               </div>
               {entry.tokens_per_second != null && entry.tokens_per_second > 0 && (
                 <span className="text-[10px] font-medium text-muted-foreground tabular-nums">
                   <span className="text-foreground">{entry.tokens_per_second}</span> <span className="text-[10px] opacity-60">T/S</span>
                 </span>
               )}
               <div className="flex-1 min-w-[40px] h-1.5 bg-muted rounded-full overflow-hidden relative">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary/40 rounded-full transition-all duration-700"
                    style={{ width: `${Math.min(100, (entry.ttft_ms! / entry.total_turn_ms!) * 100)}%` }}
                  />
               </div>
               <span className="text-[10px] font-bold text-foreground/80 tabular-nums">
                 {Math.round(entry.total_turn_ms / 100) / 10}s
               </span>
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-2xl border border-dashed border-border/80 bg-muted/20 p-4 flex items-center justify-between group animate-in zoom-in-95 duration-400">
      <div className="flex items-center gap-3">
        <div className={`size-8 rounded-xl flex items-center justify-center transition-colors ${
          entry.status === "completed" ? "bg-success/10 text-success" :
          entry.status === "executing" ? "bg-primary/10 text-primary" :
          "bg-destructive/10 text-destructive"
        }`}>
          {entry.status === "executing" ? <Spinner className="size-4" /> :
           entry.status === "completed" ? <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-4" /> :
           <HugeiconsIcon icon={Cancel01Icon} className="size-4" />}
        </div>
        <div>
          <div className="flex items-center gap-2">
            <span className="text-xs font-bold font-mono tracking-tight">{entry.tool_name}</span>
            <span className={`text-[10px] font-bold uppercase tracking-widest px-1.5 py-0.5 rounded ${
               entry.status === "completed" ? "bg-success/10 text-success" : "bg-muted text-muted-foreground"
            }`}>
              {entry.status}
            </span>
          </div>
          {entry.error_message && (
            <p className="mt-1 text-[10px] text-destructive leading-normal max-w-[400px] wrap-break-word font-medium">
              {entry.error_message}
            </p>
          )}
        </div>
      </div>
      {entry.duration_ms && (
        <div className="text-[10px] font-bold font-mono text-muted-foreground bg-background px-2 py-0.5 rounded border border-border/50 tabular-nums">
          {Math.round(entry.duration_ms)}ms
        </div>
      )}
    </div>
  );
}
