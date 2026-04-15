/**
 * Tests for the voice timeline reducer.
 *
 * Covers:
 *   - Property A: chunk sequence never increases array length while bubble open
 *   - Property B: transcript always creates new bubble if last is closed
 *   - Property C: transcript replaces open bubble (no duplication)
 *   - Classic STT-LLM-TTS path (consecutive transcripts, same role)
 *   - Gemini Live path (chunks → canonical close)
 *   - Barge-in (partial chunks → transcript close)
 *   - Mixed roles (user/assistant interleaved correctly)
 *   - Empty-text guard (empty chunks are no-ops)
 */

import { describe, it, expect } from "vitest";
import fc from "fast-check"; // property-based testing
import {
  applyChunk,
  applyTranscript,
  applyTimelineEvent,
  type TimelineEntry,
  type TranscriptChunkEvent,
  type TranscriptEvent,
} from "./timeline-reducer";

// ── helpers ───────────────────────────────────────────────────────────────────

const chunk = (role: string, text: string): TranscriptChunkEvent => ({
  type: "transcript_chunk",
  role,
  text,
});

const transcript = (role: string, text: string): TranscriptEvent => ({
  type: "transcript",
  role,
  text,
});

function lastMsg(tl: TimelineEntry[], role?: string) {
  const entries = tl.filter(
    (e): e is Extract<TimelineEntry, { kind: "message" }> =>
      e.kind === "message" && (role === undefined || e.role === role)
  );
  return entries[entries.length - 1];
}

function msgCount(tl: TimelineEntry[], role?: string) {
  return tl.filter((e) => e.kind === "message" && (role === undefined || e.role === role)).length;
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

describe("applyChunk", () => {
  it("opens a new bubble when timeline is empty", () => {
    const tl = applyChunk([], chunk("assistant", "Hello"));
    expect(tl).toHaveLength(1);
    expect(tl[0]).toMatchObject({
      kind: "message",
      role: "assistant",
      text: "Hello",
    });
    expect(tl[0]).not.toHaveProperty("_final"); // open bubble — not yet closed
  });

  it("opens a new bubble when last bubble is closed (_final: true)", () => {
    const seed: TimelineEntry[] = [
      { kind: "message", role: "assistant", text: "Prior turn.", _final: true },
    ];
    const tl = applyChunk(seed, chunk("assistant", "New"));
    expect(tl).toHaveLength(2);
    expect(tl[1]).toMatchObject({ text: "New" });
    expect(tl[1]).not.toHaveProperty("_final"); // fresh open bubble
  });

  it("appends to an existing open bubble — length is unchanged", () => {
    const seed: TimelineEntry[] = [
      {
        kind: "message",
        role: "assistant",
        text: "Hello, ",
        _final: undefined,
      },
    ];
    const tl = applyChunk(seed, chunk("assistant", "world!"));
    expect(tl).toHaveLength(1); // Property A
    expect(tl[0]).toMatchObject({ text: "Hello, world!" });
  });

  it("opens a new bubble for a different role even if previous is open", () => {
    const seed: TimelineEntry[] = [
      {
        kind: "message",
        role: "assistant",
        text: "Thinking...",
        _final: undefined,
      },
    ];
    const tl = applyChunk(seed, chunk("user", "Hello"));
    expect(tl).toHaveLength(2);
    expect(tl[1]).toMatchObject({ role: "user", text: "Hello" });
  });

  it("is a no-op for empty text", () => {
    const seed: TimelineEntry[] = [
      { kind: "message", role: "assistant", text: "A", _final: undefined },
    ];
    const tl = applyChunk(seed, chunk("assistant", ""));
    expect(tl).toBe(seed); // reference equality — no copy created
  });
});

describe("applyTranscript", () => {
  it("creates first bubble when timeline is empty", () => {
    const tl = applyTranscript([], transcript("user", "Hello!"));
    expect(tl).toHaveLength(1);
    expect(tl[0]).toMatchObject({
      kind: "message",
      role: "user",
      text: "Hello!",
      _final: true,
    });
  });

  it("Property C: closes an open streaming bubble with canonical text", () => {
    const seed: TimelineEntry[] = [
      { kind: "message", role: "user", text: "Hel", _final: undefined },
    ];
    const tl = applyTranscript(seed, transcript("user", "Hello!"));
    expect(tl).toHaveLength(1); // no new bubble
    expect(tl[0]).toMatchObject({ text: "Hello!", _final: true });
  });

  it("Property B: creates new bubble when last bubble is already closed", () => {
    const seed: TimelineEntry[] = [{ kind: "message", role: "user", text: "First.", _final: true }];
    const tl = applyTranscript(seed, transcript("user", "Second."));
    expect(tl).toHaveLength(2); // new distinct bubble
    expect(tl[1]).toMatchObject({ text: "Second.", _final: true });
  });

  it("creates new bubble when last bubble belongs to a different role", () => {
    const seed: TimelineEntry[] = [
      {
        kind: "message",
        role: "assistant",
        text: "Hi there!",
        _final: undefined,
      },
    ];
    const tl = applyTranscript(seed, transcript("user", "Hello!"));
    expect(tl).toHaveLength(2);
    expect(tl[1]).toMatchObject({ role: "user", text: "Hello!", _final: true });
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// Scenario tests
// ─────────────────────────────────────────────────────────────────────────────

describe("Gemini Live streaming scenario", () => {
  it("streams tokens then closes with canonical text — no duplicate bubble", () => {
    const events = [
      chunk("assistant", "Welcome "),
      chunk("assistant", "to "),
      chunk("assistant", "Buffalo Steak!"),
      transcript("assistant", "Welcome to Buffalo Steak!"),
    ];
    const tl = events.reduce(applyTimelineEvent, [] as TimelineEntry[]);

    expect(msgCount(tl, "assistant")).toBe(1); // exactly one bubble
    expect(lastMsg(tl, "assistant")).toMatchObject({
      text: "Welcome to Buffalo Steak!",
      _final: true,
    });
  });

  it("streams user chunks then closes with canonical — no duplicate bubble", () => {
    const events = [
      chunk("user", "Hello, "),
      chunk("user", "how's it going"),
      transcript("user", "Hello, how's it going?"),
    ];
    const tl = events.reduce(applyTimelineEvent, [] as TimelineEntry[]);

    expect(msgCount(tl, "user")).toBe(1);
    expect(lastMsg(tl, "user")).toMatchObject({
      text: "Hello, how's it going?",
      _final: true,
    });
  });

  it("handles barge-in: partial chunks then Transcript close", () => {
    const events = [
      chunk("assistant", "I can help you with "),
      chunk("assistant", "reservations—"),
      // barge-in fires; backend emits Transcript with partial accumulated text
      transcript("assistant", "I can help you with reservations—"),
    ];
    const tl = events.reduce(applyTimelineEvent, [] as TimelineEntry[]);
    expect(msgCount(tl, "assistant")).toBe(1);
    expect(lastMsg(tl, "assistant")!._final).toBe(true);
  });
});

describe("Classic STT-LLM-TTS scenario (no chunks)", () => {
  it("consecutive transcript events from same role create separate bubbles", () => {
    const tl = [
      transcript("user", "First utterance."),
      transcript("user", "Second utterance."),
    ].reduce(applyTimelineEvent, [] as TimelineEntry[]);

    expect(msgCount(tl, "user")).toBe(2); // Property B
    expect(tl[0]).toMatchObject({ text: "First utterance.", _final: true });
    expect(tl[1]).toMatchObject({ text: "Second utterance.", _final: true });
  });

  it("interleaved user/assistant transcripts maintain order", () => {
    const tl = [
      transcript("user", "Hello."),
      transcript("assistant", "Hi there!"),
      transcript("user", "How are you?"),
      transcript("assistant", "I'm doing well!"),
    ].reduce(applyTimelineEvent, [] as TimelineEntry[]);

    expect(tl).toHaveLength(4);
    expect(tl.map((e) => (e as { role: string }).role)).toEqual([
      "user",
      "assistant",
      "user",
      "assistant",
    ]);
    expect(tl.every((e) => (e as { _final?: boolean })._final === true)).toBe(true);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// Property-based tests (fast-check)
// ─────────────────────────────────────────────────────────────────────────────

describe("Property A – chunk sequence never creates extra bubbles", () => {
  it("holds for any sequence of same-role non-empty chunks", () => {
    fc.assert(
      fc.property(
        fc.array(fc.string({ minLength: 1, maxLength: 20 }), {
          minLength: 1,
          maxLength: 50,
        }),
        fc.constantFrom("user", "assistant"),
        (tokens, role) => {
          const events = tokens.map((t) => chunk(role, t));
          const tl = events.reduce(applyChunk, [] as TimelineEntry[]);
          // Only one bubble should exist for this role.
          expect(msgCount(tl, role)).toBe(1);
        }
      )
    );
  });
});

describe("Property B – transcript always closes or creates, never duplicates open", () => {
  it("holds: canonical transcript after N chunks yields exactly 1 bubble", () => {
    fc.assert(
      fc.property(
        fc.array(fc.string({ minLength: 1, maxLength: 10 }), {
          minLength: 0,
          maxLength: 30,
        }),
        fc.string({ minLength: 1, maxLength: 50 }),
        fc.constantFrom("user", "assistant"),
        (tokens, finalText, role) => {
          let tl: TimelineEntry[] = [];
          for (const t of tokens) tl = applyChunk(tl, chunk(role, t));
          tl = applyTranscript(tl, transcript(role, finalText));

          const msgs = tl.filter((e) => e.kind === "message" && e.role === role);
          // Exactly one message bubble for this role.
          expect(msgs).toHaveLength(1);
          // It must be closed with the canonical text.
          expect(msgs[0]).toMatchObject({ text: finalText, _final: true });
        }
      )
    );
  });
});

describe("Property C – consecutive transcripts from same role never collapse", () => {
  it("N canonical transcripts → exactly N distinct closed bubbles", () => {
    fc.assert(
      fc.property(
        fc.array(fc.string({ minLength: 1, maxLength: 30 }), {
          minLength: 1,
          maxLength: 20,
        }),
        fc.constantFrom("user", "assistant"),
        (texts, role) => {
          const tl = texts
            .map((t) => transcript(role, t))
            .reduce(applyTimelineEvent, [] as TimelineEntry[]);

          expect(msgCount(tl, role)).toBe(texts.length);
          expect(tl.every((e) => e.kind !== "message" || e._final === true)).toBe(true);
        }
      )
    );
  });
});
