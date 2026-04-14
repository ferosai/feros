/**
 * Pure timeline state reducer for the voice test view.
 *
 * Extracted from manual-test-view.tsx so we can unit- and property-test
 * every merging scenario without a browser harness or React renderer.
 *
 * Rules
 * ──────
 * • `transcript_chunk` (is_final always false from backend):
 *     – Appends tokens to an *open* (not _final) bubble of the same role, OR
 *     – Opens a fresh bubble if none exists / the last one is already closed.
 *
 * • `transcript` (canonical, always _final: true):
 *     – Replaces an open bubble of the same role with the canonical text, OR
 *     – Creates a new closed bubble if no open bubble exists (classic pipeline
 *       path where transcripts arrive without prior chunks).
 */

export type MessageTimelineEntry = {
  kind: "message";
  role: string;
  text: string;
  _final?: boolean;
  ttft_ms?: number | null;
  first_sentence_ms?: number | null;
  total_turn_ms?: number;
  tokens_per_second?: number;
};

export type ToolTimelineEntry = {
  kind: "tool";
  tool_call_id?: string;
  tool_name: string;
  status: "executing" | "completed" | "error" | "interrupted" | "orphaned";
  duration_ms?: number;
  timestamp: number;
  error_message?: string;
};

export type TimelineEntry = MessageTimelineEntry | ToolTimelineEntry;

// ── Event shapes ──────────────────────────────────────────────────────────────

export type TranscriptChunkEvent = {
  type: "transcript_chunk";
  role: string;
  text: string;
  // From the backend this is always false (see session.rs). Present here for
  // forward-compatibility but the reducer ignores it.
  is_final?: boolean;
};

export type TranscriptEvent = {
  type: "transcript";
  role: string;
  text: string;
};

export type TimelineEvent = TranscriptChunkEvent | TranscriptEvent;

// ── Helpers ───────────────────────────────────────────────────────────────────

function isOpenBubble(entry: TimelineEntry, role: string): entry is MessageTimelineEntry {
  return entry.kind === "message" && entry.role === role && !entry._final;
}

// ── Reducers ──────────────────────────────────────────────────────────────────

/**
 * Apply a `transcript_chunk` event (non-final streaming token).
 *
 * Property A: Never increases the array length when an open bubble exists.
 */
export function applyChunk(
  prev: TimelineEntry[],
  event: TranscriptChunkEvent,
): TimelineEntry[] {
  const { role, text } = event;
  if (!text) return prev; // empty chunk is a no-op

  const last = prev[prev.length - 1];
  if (last && isOpenBubble(last, role)) {
    // Append to existing open bubble — no length change.
    const updated = [...prev];
    updated[updated.length - 1] = { ...last, text: last.text + text };
    return updated;
  }

  // Open a new streaming bubble.
  return [...prev, { kind: "message" as const, role, text }];
}

/**
 * Apply a `transcript` event (canonical, always final).
 *
 * Property B: Always creates a new bubble unless there is an open bubble.
 * Property C: Replaces an open bubble of the same role instead of duplicating.
 */
export function applyTranscript(
  prev: TimelineEntry[],
  event: TranscriptEvent,
): TimelineEntry[] {
  const { role, text } = event;
  const lastIdx = prev.length - 1;
  const last = prev[lastIdx];

  if (last && isOpenBubble(last, role)) {
    // Replace streaming bubble with the canonical text and close it.
    const updated = [...prev];
    updated[lastIdx] = { ...last, text, _final: true };
    return updated;
  }

  // Classic pipeline or first bubble: just append a new closed entry.
  return [...prev, { kind: "message" as const, role, text, _final: true }];
}

/**
 * Convenience: route any recognized timeline event to the correct reducer.
 */
export function applyTimelineEvent(
  prev: TimelineEntry[],
  event: TimelineEvent,
): TimelineEntry[] {
  if (event.type === "transcript_chunk") return applyChunk(prev, event);
  if (event.type === "transcript") return applyTranscript(prev, event);
  return prev;
}
