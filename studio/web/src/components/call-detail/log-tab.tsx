import { useMemo, useState } from "react";
import type { CallEvent, CallExternalLink } from "@/lib/api/client";
import { Badge } from "@/components/ui/badge";

type StageKind = "stt" | "llm" | "tts";

type StageEntry = {
  seq: number;
  occurredAt: string;
  durationMs: number;
  payload: Record<string, unknown>;
};

type TurnView = {
  key: string;
  turnNumber: number;
  startSeq: number;
  endSeq: number;
  startAt: string | null;
  endAt: string | null;
  turnDurationMs: number | null;
  stt: StageEntry[];
  llm: StageEntry[];
  tts: StageEntry[];
  errors: CallEvent[];
  tools: CallEvent[];
};

type ToolTiming = {
  seq: number;
  occurredAt: string;
  toolName: string;
  status: string;
  durationMs: number;
};

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object") return null;
  return value as Record<string, unknown>;
}

function asNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const n = Number(value);
    if (Number.isFinite(n)) return n;
  }
  return null;
}

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function formatMs(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0ms";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
  const wholeSec = Math.round(ms / 1000);
  const min = Math.floor(wholeSec / 60);
  const sec = wholeSec % 60;
  return `${min}:${String(sec).padStart(2, "0")}`;
}

function stageDuration(turn: TurnView, kind: StageKind): number {
  return turn[kind].reduce((acc, item) => acc + item.durationMs, 0);
}

function turnTotalMs(turn: TurnView, toolDurationMs = 0): number {
  if (turn.turnDurationMs && turn.turnDurationMs > 0) return turn.turnDurationMs;
  return (
    stageDuration(turn, "stt") +
    stageDuration(turn, "llm") +
    stageDuration(turn, "tts") +
    toolDurationMs
  );
}

function stageColor(kind: StageKind): string {
  if (kind === "stt") return "bg-sky-500/80";
  if (kind === "llm") return "bg-amber-500/80";
  return "bg-emerald-500/80";
}

function toolColor(): string {
  return "bg-fuchsia-500/80";
}

function parseTimeMs(value: string): number {
  const t = Date.parse(value);
  return Number.isFinite(t) ? t : 0;
}

function toolTimingsForTurn(turn: TurnView): ToolTiming[] {
  const sorted = [...turn.tools].sort((a, b) => a.seq - b.seq);
  const startByName = new Map<string, number[]>();
  const out: ToolTiming[] = [];

  for (const event of sorted) {
    const payload = asRecord(event.payload_json) ?? {};
    const toolName = asString(payload.tool_name) ?? "tool: —";
    const status = (asString(payload.status) ?? "status: —").toLowerCase();
    const at = parseTimeMs(event.occurred_at);

    let durationMs = asNumber(payload.duration_ms) ?? 0;
    const looksStart =
      status === "started" || status === "start" || status === "running" || status === "executing";
    const looksEnd =
      status === "completed" ||
      status === "success" ||
      status === "failed" ||
      status === "error" ||
      status === "cancelled";

    if (durationMs <= 0 && looksStart) {
      const queue = startByName.get(toolName) ?? [];
      queue.push(at);
      startByName.set(toolName, queue);
    }

    if (durationMs <= 0 && looksEnd) {
      const queue = startByName.get(toolName) ?? [];
      const startedAt = queue.shift();
      if (startedAt && at > startedAt) {
        durationMs = at - startedAt;
      }
      startByName.set(toolName, queue);
    }

    out.push({
      seq: event.seq,
      occurredAt: event.occurred_at,
      toolName,
      status: asString(payload.status) ?? "status: —",
      durationMs: Math.max(0, durationMs),
    });
  }
  return out;
}

function summarizeTurns(events: CallEvent[]): TurnView[] {
  const sorted = [...events].sort((a, b) => a.seq - b.seq);
  const turns: TurnView[] = [];
  const numberedTurns = new Map<number, TurnView>();
  let activeTurnIndex = -1;
  let syntheticTurn = -1;
  let greetingTurn: TurnView | null = null;

  const ensureActiveTurn = (event: CallEvent) => {
    if (activeTurnIndex >= 0) return turns[activeTurnIndex];
    syntheticTurn -= 1;
    const t: TurnView = {
      key: `synthetic-${Math.abs(syntheticTurn)}-${event.seq}`,
      turnNumber: syntheticTurn,
      startSeq: event.seq,
      endSeq: event.seq,
      startAt: event.occurred_at,
      endAt: null,
      turnDurationMs: null,
      stt: [],
      llm: [],
      tts: [],
      errors: [],
      tools: [],
    };
    turns.push(t);
    activeTurnIndex = turns.length - 1;
    return t;
  };

  const getOrCreateTurnByNumber = (turnNumber: number, event: CallEvent): TurnView => {
    const existing = numberedTurns.get(turnNumber);
    if (existing) return existing;
    const created: TurnView = {
      key: `turn-${turnNumber}`,
      turnNumber,
      startSeq: event.seq,
      endSeq: event.seq,
      startAt: event.occurred_at,
      endAt: null,
      turnDurationMs: null,
      stt: [],
      llm: [],
      tts: [],
      errors: [],
      tools: [],
    };
    turns.push(created);
    numberedTurns.set(turnNumber, created);
    return created;
  };

  const getOrCreateGreetingTurn = (event: CallEvent): TurnView => {
    if (greetingTurn) return greetingTurn;
    const created: TurnView = {
      key: "turn-0-greeting",
      turnNumber: 0,
      startSeq: event.seq,
      endSeq: event.seq,
      startAt: event.occurred_at,
      endAt: null,
      turnDurationMs: null,
      stt: [],
      llm: [],
      tts: [],
      errors: [],
      tools: [],
    };
    turns.push(created);
    greetingTurn = created;
    return created;
  };

  for (const event of sorted) {
    const payload = asRecord(event.payload_json);
    const payloadType = asString(payload?.type);
    const eventType = payloadType ?? event.event_type;
    const payloadTurnNumber = asNumber(payload?.turn_number);

    if (eventType === "turn_started") {
      const turnNumber = payloadTurnNumber ?? turns.filter((t) => t.turnNumber > 0).length + 1;
      const t = getOrCreateTurnByNumber(turnNumber, event);
      t.startSeq = Math.min(t.startSeq, event.seq);
      t.startAt = t.startAt ?? event.occurred_at;
      activeTurnIndex = turns.length - 1;
      continue;
    }

    if (eventType === "turn_ended") {
      const turnNumber = payloadTurnNumber;
      let target = activeTurnIndex >= 0 ? turns[activeTurnIndex] : null;
      if (turnNumber != null) {
        target = numberedTurns.get(turnNumber) ?? target;
      }
      if (target) {
        target.endSeq = event.seq;
        target.endAt = event.occurred_at;
        target.turnDurationMs = asNumber(payload?.turn_duration_ms) ?? target.turnDurationMs;
        const endedIndex = turns.findIndex((t) => t.key === target?.key);
        if (endedIndex >= 0 && endedIndex === activeTurnIndex) activeTurnIndex = -1;
      }
      continue;
    }

    let turn: TurnView;
    if (payloadTurnNumber != null && payloadTurnNumber > 0) {
      turn = getOrCreateTurnByNumber(payloadTurnNumber, event);
      activeTurnIndex = turns.findIndex((t) => t.key === turn.key);
    } else if (eventType === "tts_complete" && turns.every((t) => t.turnNumber <= 0)) {
      turn = getOrCreateGreetingTurn(event);
    } else {
      turn = ensureActiveTurn(event);
    }
    turn.endSeq = event.seq;
    turn.endAt = event.occurred_at;

    if (eventType === "stt_complete") {
      turn.stt.push({
        seq: event.seq,
        occurredAt: event.occurred_at,
        durationMs: asNumber(payload?.duration_ms) ?? 0,
        payload: payload ?? {},
      });
      continue;
    }
    if (eventType === "llm_complete") {
      turn.llm.push({
        seq: event.seq,
        occurredAt: event.occurred_at,
        durationMs: asNumber(payload?.duration_ms) ?? 0,
        payload: payload ?? {},
      });
      continue;
    }
    if (eventType === "tts_complete") {
      turn.tts.push({
        seq: event.seq,
        occurredAt: event.occurred_at,
        durationMs: asNumber(payload?.duration_ms) ?? 0,
        payload: payload ?? {},
      });
      continue;
    }
    if (eventType === "error") {
      turn.errors.push(event);
      continue;
    }
    if (eventType === "tool_activity") {
      turn.tools.push(event);
    }
  }

  return turns;
}

function normalizeStageDurations(
  turns: TurnView[],
  toolDurationByTurn: Record<string, number>,
  totalDurationSec?: number | null
): Record<string, { stt: number; llm: number; tts: number; tool: number; total: number }> {
  const rawByTurn: Record<
    string,
    { stt: number; llm: number; tts: number; tool: number; total: number }
  > = {};
  let rawTotal = 0;
  for (const turn of turns) {
    const stt = stageDuration(turn, "stt");
    const llm = stageDuration(turn, "llm");
    const tts = stageDuration(turn, "tts");
    const tool = toolDurationByTurn[turn.key] ?? 0;
    const total = stt + llm + tts + tool;
    rawByTurn[turn.key] = { stt, llm, tts, tool, total };
    rawTotal += total;
  }
  const targetTotalMs =
    (totalDurationSec ?? 0) > 0 ? (totalDurationSec as number) * 1000 : rawTotal;
  if (targetTotalMs <= 0 || rawTotal <= 0) return rawByTurn;

  const scale = targetTotalMs / rawTotal;
  const out: Record<
    string,
    { stt: number; llm: number; tts: number; tool: number; total: number }
  > = {};
  for (const turn of turns) {
    const raw = rawByTurn[turn.key];
    const stt = raw.stt * scale;
    const llm = raw.llm * scale;
    const tts = raw.tts * scale;
    const tool = raw.tool * scale;
    out[turn.key] = { stt, llm, tts, tool, total: stt + llm + tts + tool };
  }
  return out;
}

export function LogTab({
  hasInternalLogs,
  events,
  eventsLoading,
  eventsError,
  externalLinks,
  totalDurationSec,
}: {
  hasInternalLogs: boolean;
  events: CallEvent[];
  eventsLoading: boolean;
  eventsError: string | null;
  externalLinks: CallExternalLink[];
  totalDurationSec?: number | null;
}) {
  const turns = useMemo(() => summarizeTurns(events), [events]);
  const toolTimingsByTurn = useMemo(() => {
    const out: Record<string, ToolTiming[]> = {};
    for (const turn of turns) out[turn.key] = toolTimingsForTurn(turn);
    return out;
  }, [turns]);
  const toolDurationByTurn = useMemo(() => {
    const out: Record<string, number> = {};
    for (const turn of turns) {
      out[turn.key] = (toolTimingsByTurn[turn.key] ?? []).reduce(
        (acc, item) => acc + item.durationMs,
        0
      );
    }
    return out;
  }, [turns, toolTimingsByTurn]);
  const [selectedTurnKey, setSelectedTurnKey] = useState<string | null>(null);
  const selectedTurn = useMemo(
    () => turns.find((t) => t.key === selectedTurnKey) ?? null,
    [turns, selectedTurnKey]
  );
  const normalizedByTurn = useMemo(
    () => normalizeStageDurations(turns, toolDurationByTurn, totalDurationSec),
    [turns, toolDurationByTurn, totalDurationSec]
  );
  const visibleTurns = useMemo(() => {
    const MIN_UNLABELED_MS = 200;
    return turns.filter((turn) => {
      if (turn.turnNumber > 0) return true;
      const total =
        normalizedByTurn[turn.key]?.total ?? turnTotalMs(turn, toolDurationByTurn[turn.key] ?? 0);
      return total > MIN_UNLABELED_MS;
    });
  }, [turns, normalizedByTurn, toolDurationByTurn]);

  const timelineTotalMs = useMemo(() => {
    if (totalDurationSec && totalDurationSec > 0) return totalDurationSec * 1000;
    const byTurns = visibleTurns.reduce((acc, t) => acc + (normalizedByTurn[t.key]?.total ?? 0), 0);
    return byTurns > 0 ? byTurns : 1;
  }, [visibleTurns, totalDurationSec, normalizedByTurn]);

  if (!hasInternalLogs && externalLinks.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-border/60 p-10 text-center">
        <p className="text-sm text-foreground font-medium">Log tab is disabled</p>
        <p className="text-xs text-muted-foreground mt-1">
          No observability adapter is active for this call.
        </p>
      </div>
    );
  }

  if (!hasInternalLogs && externalLinks.length > 0) {
    return (
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">This call has no internal event logs.</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {eventsLoading ? (
        <div className="rounded-lg border border-border/60 p-6 text-xs text-muted-foreground">
          Loading call events...
        </div>
      ) : null}

      {!eventsLoading && eventsError ? (
        <div className="rounded-lg border border-dashed border-border/60 p-6 text-xs text-muted-foreground">
          {eventsError}
        </div>
      ) : null}

      {!eventsLoading && !eventsError && events.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border/60 p-6 text-xs text-muted-foreground">
          No internal call events found.
        </div>
      ) : null}

      {!eventsLoading && !eventsError && events.length > 0 ? (
        <div className="space-y-4">
          <div className="rounded-lg border border-border/60 p-3">
            <div className="mb-2 flex items-center justify-between">
              <p className="text-xs font-medium text-foreground">Turn Timeline</p>
              <p className="text-[10px] text-muted-foreground">Total {formatMs(timelineTotalMs)}</p>
            </div>
            <div className="h-3 w-full overflow-hidden rounded-full bg-secondary/40">
              <div className="flex h-full w-full">
                {visibleTurns.map((turn) => {
                  const normalized = normalizedByTurn[turn.key] ?? {
                    stt: 0,
                    llm: 0,
                    tts: 0,
                    tool: 0,
                    total: 1,
                  };
                  const total = Math.max(1, normalized.total);
                  const widthPct = Math.max(4, (total / timelineTotalMs) * 100);
                  const sttMs = normalized.stt;
                  const llmMs = normalized.llm;
                  const ttsMs = normalized.tts;
                  const toolMs = normalized.tool;
                  const selected = selectedTurnKey === turn.key;
                  const muted = selectedTurnKey != null && !selected;
                  const TOOL_SEGMENT_MIN_MS = 1000;
                  const showToolInTimeline = toolMs >= TOOL_SEGMENT_MIN_MS;
                  const totalStage = Math.max(
                    1,
                    sttMs + llmMs + ttsMs + (showToolInTimeline ? toolMs : 0)
                  );
                  return (
                    <button
                      key={turn.key}
                      type="button"
                      title={`Turn ${turn.turnNumber}`}
                      onClick={() =>
                        setSelectedTurnKey((cur) => (cur === turn.key ? null : turn.key))
                      }
                      className={`h-full ${muted ? "opacity-35" : "opacity-100"} transition-opacity`}
                      style={{ width: `${widthPct}%` }}
                    >
                      <div className="flex h-full w-full">
                        <div
                          className={`${stageColor("stt")} h-full`}
                          style={{ width: `${(sttMs / totalStage) * 100}%` }}
                        />
                        <div
                          className={`${stageColor("llm")} h-full`}
                          style={{ width: `${(llmMs / totalStage) * 100}%` }}
                        />
                        <div
                          className={`${stageColor("tts")} h-full`}
                          style={{ width: `${(ttsMs / totalStage) * 100}%` }}
                        />
                        {showToolInTimeline ? (
                          <div
                            className={`${toolColor()} h-full`}
                            style={{ width: `${(toolMs / totalStage) * 100}%` }}
                          />
                        ) : null}
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
            <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[10px] text-muted-foreground">
              <span className="inline-flex items-center gap-1">
                <span className="h-2 w-2 rounded-full bg-sky-500/80" /> STT
              </span>
              <span className="inline-flex items-center gap-1">
                <span className="h-2 w-2 rounded-full bg-amber-500/80" /> LLM
              </span>
              <span className="inline-flex items-center gap-1">
                <span className="h-2 w-2 rounded-full bg-emerald-500/80" /> TTS
              </span>
              <span className="inline-flex items-center gap-1">
                <span className="h-2 w-2 rounded-full bg-fuchsia-500/80" /> Tool Call
              </span>
            </div>
          </div>

          {visibleTurns.map((turn) => {
            const normalized = normalizedByTurn[turn.key] ?? {
              stt: 0,
              llm: 0,
              tts: 0,
              tool: 0,
              total: 0,
            };
            const sttMs = normalized.stt;
            const llmMs = normalized.llm;
            const ttsMs = normalized.tts;
            const toolMs = normalized.tool;
            const total = normalized.total > 0 ? normalized.total : turnTotalMs(turn, toolMs);
            const toolTimings = toolTimingsByTurn[turn.key] ?? [];
            const expanded = selectedTurn?.key === turn.key;
            return (
              <div key={turn.key} className="rounded-lg border border-border/60">
                <button
                  type="button"
                  onClick={() => setSelectedTurnKey((cur) => (cur === turn.key ? null : turn.key))}
                  className={`w-full px-3 py-2 text-left ${expanded ? "bg-secondary/25" : "hover:bg-secondary/20"}`}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium text-foreground">
                        {turn.turnNumber > 0
                          ? `Turn ${turn.turnNumber}`
                          : turn.turnNumber === 0
                            ? "Turn 0 (Greeting)"
                            : "Unlabeled Turn"}
                      </span>
                      {turn.turnNumber === 0 ? (
                        <Badge
                          variant="outline"
                          className="h-5 px-1.5 text-[10px] uppercase tracking-wide"
                        >
                          Greeting
                        </Badge>
                      ) : null}
                      <span className="text-[10px] text-muted-foreground">
                        #{turn.startSeq} - #{turn.endSeq}
                      </span>
                    </div>
                    <span className="text-xs text-foreground">{formatMs(total)}</span>
                  </div>
                  <div className="mt-1 grid grid-cols-4 gap-2 text-[10px] text-muted-foreground">
                    <div>STT: {formatMs(sttMs)}</div>
                    <div>LLM: {llmMs > 0 ? formatMs(llmMs) : "N/A"}</div>
                    <div>TTS: {ttsMs > 0 || turn.tts.length > 0 ? formatMs(ttsMs) : "N/A"}</div>
                    <div>
                      Tool: {formatMs(toolMs)} ({turn.tools.length})
                    </div>
                  </div>
                </button>

                {expanded ? (
                  <div className="border-t border-border/60 px-3 py-3 text-xs">
                    <div className="grid gap-3 md:grid-cols-4">
                      <div className="space-y-1">
                        <p className="font-medium text-foreground">STT ({turn.stt.length})</p>
                        {turn.stt.length === 0 ? (
                          <p className="text-muted-foreground">No STT event</p>
                        ) : (
                          turn.stt.map((e) => (
                            <div
                              key={`stt-${e.seq}`}
                              className="rounded border border-border/50 p-2 text-[10px]"
                            >
                              <p className="text-muted-foreground">{formatMs(e.durationMs)}</p>
                              <p className="mt-1 line-clamp-3 wrap-break-word text-foreground">
                                {(asString(e.payload.transcript) ?? "").trim() || "—"}
                              </p>
                            </div>
                          ))
                        )}
                      </div>

                      <div className="space-y-1">
                        <p className="font-medium text-foreground">LLM ({turn.llm.length})</p>
                        {turn.llm.length === 0 ? (
                          <p className="text-muted-foreground">No LLM event</p>
                        ) : (
                          turn.llm.map((e) => {
                            const promptTokens = asNumber(e.payload.prompt_tokens) ?? 0;
                            const compTokens = asNumber(e.payload.completion_tokens) ?? 0;
                            const hasTokens = promptTokens > 0 || compTokens > 0;
                            const hasDuration = e.durationMs > 0;
                            return (
                              <div
                                key={`llm-${e.seq}`}
                                className="rounded border border-border/50 p-2 text-[10px]"
                              >
                                <p className="text-muted-foreground">
                                  {hasDuration ? formatMs(e.durationMs) : "Duration N/A"}
                                  {(hasTokens || hasDuration) && " · "}
                                  {hasTokens
                                    ? `tokens ${promptTokens}/${compTokens}`
                                    : "tokens N/A"}
                                </p>
                                <p className="mt-1 wrap-break-word text-foreground">
                                  {asString(e.payload.model) ?? "model: —"}
                                </p>
                              </div>
                            );
                          })
                        )}
                      </div>

                      <div className="space-y-1">
                        <p className="font-medium text-foreground">TTS ({turn.tts.length})</p>
                        {turn.tts.length === 0 ? (
                          <p className="text-muted-foreground">No TTS event</p>
                        ) : (
                          turn.tts.map((e) => (
                            <div
                              key={`tts-${e.seq}`}
                              className="rounded border border-border/50 p-2 text-[10px]"
                            >
                              <p className="text-muted-foreground">
                                {formatMs(e.durationMs)} · chars{" "}
                                {asNumber(e.payload.character_count) ?? 0}
                              </p>
                              <p className="mt-1 line-clamp-3 wrap-break-word text-foreground">
                                {(asString(e.payload.text) ?? "").trim() || "—"}
                              </p>
                            </div>
                          ))
                        )}
                      </div>

                      <div className="space-y-1">
                        <p className="font-medium text-foreground">TOOL ({turn.tools.length})</p>
                        {toolTimings.length === 0 ? (
                          <p className="text-muted-foreground">No Tool event</p>
                        ) : (
                          toolTimings.map((e) => {
                            return (
                              <div
                                key={`tool-${e.seq}`}
                                className="rounded border border-border/50 p-2 text-[10px]"
                              >
                                <p className="text-muted-foreground">
                                  #{e.seq} · {new Date(e.occurredAt).toLocaleTimeString()} ·{" "}
                                  {formatMs(e.durationMs)}
                                </p>
                                <p className="mt-1 wrap-break-word text-foreground">{e.toolName}</p>
                                <p className="mt-0.5 text-muted-foreground">{e.status}</p>
                              </div>
                            );
                          })
                        )}
                      </div>
                    </div>

                    {turn.errors.length > 0 ? (
                      <div className="mt-3 grid gap-2 md:grid-cols-1">
                        <div className="rounded border border-border/50 p-2 text-[10px]">
                          <p className="font-medium text-foreground">Errors</p>
                          <p className="text-muted-foreground mt-1">{turn.errors.length}</p>
                        </div>
                      </div>
                    ) : null}
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}
