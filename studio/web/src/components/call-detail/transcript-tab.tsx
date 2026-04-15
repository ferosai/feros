import { HugeiconsIcon } from "@hugeicons/react";
import { MessageSquareDashedIcon } from "@hugeicons/core-free-icons";

import type { TranscriptMessage } from "./types";
import { formatDateTime } from "./types";

export function TranscriptTab({
  messages,
  activeIndex,
}: {
  messages: TranscriptMessage[];
  activeIndex: number;
}) {
  if (messages.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-border/60 p-10 text-center">
        <HugeiconsIcon
          icon={MessageSquareDashedIcon}
          className="mx-auto size-8 text-muted-foreground/50"
        />
        <p className="mt-3 text-sm text-muted-foreground">
          No transcript messages were captured for this call.
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {messages.map((msg, idx) => {
        const isUser = msg.role.toLowerCase() === "user";
        const isActive = idx === activeIndex;

        return (
          <div
            key={msg.id}
            className={`rounded-lg border px-3.5 py-3 transition-colors ${
              isActive ? "border-primary/50 bg-primary/5" : "border-border/50 bg-secondary/20"
            }`}
          >
            <div className="mb-1.5 flex items-center justify-between gap-3">
              <span
                className={`text-[10px] font-semibold uppercase tracking-wide ${
                  isUser ? "text-primary" : "text-foreground"
                }`}
              >
                {msg.role}
              </span>
              <span className="text-[10px] text-muted-foreground">
                {formatDateTime(msg.timestamp)}
              </span>
            </div>
            <p className="whitespace-pre-wrap text-sm text-foreground">{msg.text}</p>
          </div>
        );
      })}
    </div>
  );
}
