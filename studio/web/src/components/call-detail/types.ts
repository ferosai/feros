export type TranscriptMessage = {
  id: string;
  role: string;
  text: string;
  timestamp: string | null;
  offsetSecRaw: number | null;
};

export type TranscriptDoc = {
  started_at: string | null;
  ended_at: string | null;
  wallclockDurationSec: number | null;
  entries: Array<Record<string, unknown>>;
};

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

export function parseTranscript(raw: unknown): TranscriptDoc | null {
  if (!isObject(raw)) return null;
  const entriesRaw = raw.entries;
  if (!Array.isArray(entriesRaw)) return null;

  const startedAt =
    typeof raw.started_at === "string" && raw.started_at.length > 0 ? raw.started_at : null;
  const endedAt = typeof raw.ended_at === "string" && raw.ended_at.length > 0 ? raw.ended_at : null;

  let wallclockDurationSec: number | null = null;
  if (startedAt && endedAt) {
    const startMs = Date.parse(startedAt);
    const endMs = Date.parse(endedAt);
    if (Number.isFinite(startMs) && Number.isFinite(endMs) && endMs > startMs) {
      wallclockDurationSec = (endMs - startMs) / 1000;
    }
  }

  const entries = entriesRaw.filter(isObject);
  return {
    started_at: startedAt,
    ended_at: endedAt,
    wallclockDurationSec,
    entries,
  };
}

export function extractTranscriptMessages(doc: TranscriptDoc | null): TranscriptMessage[] {
  if (!doc) return [];
  const sessionStartMs = doc.started_at ? Date.parse(doc.started_at) : Number.NaN;

  return doc.entries
    .filter((entry) => entry.type === "message")
    .map((entry, idx) => {
      const role = typeof entry.role === "string" ? entry.role : "unknown";
      const text = typeof entry.text === "string" ? entry.text : "";
      const timestamp = typeof entry.timestamp === "string" ? entry.timestamp : null;

      let offsetSec: number | null = null;
      if (timestamp && Number.isFinite(sessionStartMs)) {
        const tsMs = Date.parse(timestamp);
        if (Number.isFinite(tsMs) && tsMs >= sessionStartMs) {
          offsetSec = (tsMs - sessionStartMs) / 1000;
        }
      }

      return {
        id: `${idx}-${timestamp ?? "no-ts"}`,
        role,
        text,
        timestamp,
        offsetSecRaw: offsetSec,
      };
    })
    .filter((msg) => msg.text.trim().length > 0)
    .sort((a, b) => {
      if (a.offsetSecRaw !== null && b.offsetSecRaw !== null) {
        return a.offsetSecRaw - b.offsetSecRaw;
      }
      if (a.offsetSecRaw !== null) return -1;
      if (b.offsetSecRaw !== null) return 1;
      if (a.timestamp && b.timestamp) return a.timestamp.localeCompare(b.timestamp);
      return 0;
    });
}

export function formatDuration(totalSec: number | null | undefined): string {
  if (totalSec == null || !Number.isFinite(totalSec)) return "—";
  const s = Math.max(0, Math.floor(totalSec));
  const mins = Math.floor(s / 60);
  const secs = s % 60;
  return `${mins}:${String(secs).padStart(2, "0")}`;
}

export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleString([], {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
