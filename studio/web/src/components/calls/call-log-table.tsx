"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowRight01Icon,
  CallDisabled02Icon,
  FileHeadphoneIcon,
  MicOff01Icon,
  PauseIcon,
  PlayIcon,
} from "@hugeicons/core-free-icons";
import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { useParams } from "next/navigation";

import { Badge } from "@/components/ui/badge";
import { API_BASE, type CallLog } from "@/lib/api/client";

function formatDuration(durationSeconds: number | null): string {
  if (durationSeconds === null) return "—";
  return `${Math.floor(durationSeconds / 60)}:${String(durationSeconds % 60).padStart(2, "0")}`;
}

function shortAgentId(agentId: string): string {
  return agentId.slice(0, 8);
}

function getAbsoluteUrl(url: string | null): string | undefined {
  if (!url) return undefined;
  return url.startsWith("/") ? `${API_BASE}${url}` : url;
}

type CallLogTableProps = {
  calls: CallLog[];
  loading: boolean;
  agentNameById?: Record<string, string>;
  showColumnHeader?: boolean;
  loadingRows?: number;
  emptyTitle?: string;
  emptyDescription?: string;
};

export function CallLogTable({
  calls,
  loading,
  agentNameById = {},
  showColumnHeader = true,
  loadingRows = 5,
  emptyTitle = "No calls yet",
  emptyDescription = "Call logs will appear here once your agents start handling traffic.",
}: CallLogTableProps) {
  const { workspace_slug } = useParams<{ workspace_slug: string }>();
  const [playingId, setPlayingId] = useState<string | null>(null);
  const audioRefs = useRef<Record<string, HTMLAudioElement | null>>({});

  useEffect(() => {
    const currentRefs = audioRefs.current;
    return () => {
      Object.values(currentRefs).forEach((el) => {
        if (el) el.pause();
      });
    };
  }, []);

  return (
    <div className="flat-card overflow-hidden">
      {showColumnHeader ? (
        <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,220px)_96px_80px_20px] gap-4 px-5 py-3 text-xs text-muted-foreground border-b border-border/30">
          <span className="pl-11">Agent</span>
          <span>Time / Duration</span>
          <span className="w-24 text-center">Recording</span>
          <span className="w-20 text-right">Status</span>
          <span aria-hidden="true" />
        </div>
      ) : null}

      {loading ? (
        <div className="space-y-px">
          {Array.from({ length: loadingRows }).map((_, i) => (
            <div key={i} className="h-16 bg-secondary/30 animate-pulse" />
          ))}
        </div>
      ) : calls.length === 0 ? (
        <div className="py-20 text-center">
          <div className="size-16 rounded-2xl bg-secondary flex items-center justify-center mx-auto mb-5">
            <HugeiconsIcon icon={CallDisabled02Icon} className="size-7 text-muted-foreground/40" />
          </div>
          <h3 className="text-sm font-semibold text-foreground mb-1.5">{emptyTitle}</h3>
          <p className="text-sm text-muted-foreground max-w-[280px] mx-auto">{emptyDescription}</p>
        </div>
      ) : (
        <div>
          {calls.map((call, idx) => (
            <div
              key={call.id}
              className={`group grid grid-cols-[minmax(0,1fr)_minmax(0,220px)_96px_80px_20px] gap-4 items-center px-5 py-3.5 hover:bg-secondary/40 transition-colors ${idx > 0 ? "border-t border-border/30" : ""}`}
            >
              <Link
                href={`/dashboard/${workspace_slug}/calls/${call.id}`}
                className="block min-w-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm"
              >
                <div className="flex items-center gap-3 min-w-0">
                  <div className="size-9 rounded-lg flex items-center justify-center bg-secondary text-muted-foreground group-hover:bg-primary/10 group-hover:text-primary transition-colors">
                    <HugeiconsIcon icon={FileHeadphoneIcon} className="size-4" />
                  </div>
                  <div className="min-w-0">
                    <p className="text-xs font-medium text-foreground truncate">
                      {call.agent_name ?? agentNameById[call.agent_id] ?? "Unknown agent"}
                    </p>
                    <p className="text-[10px] text-muted-foreground font-mono truncate">
                      Agent {shortAgentId(call.agent_id)}
                    </p>
                  </div>
                  <Badge
                    variant="outline"
                    className="ml-2 h-5 px-1.5 text-[10px] uppercase tracking-wide shrink-0"
                  >
                    {call.direction}
                  </Badge>
                </div>
              </Link>

              <Link
                href={`/dashboard/${workspace_slug}/calls/${call.id}`}
                className="block text-xs text-muted-foreground truncate focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm"
              >
                {call.started_at
                  ? new Date(call.started_at).toLocaleString([], {
                      hour: "2-digit",
                      minute: "2-digit",
                      month: "short",
                      day: "numeric",
                    })
                  : "—"}
                <span className="mx-1">·</span>
                <span className="font-mono">{formatDuration(call.duration_seconds)}</span>
              </Link>

              <div className="w-24 flex justify-center">
                {call.recording_url ? (
                  <>
                    <audio
                      preload="none"
                      ref={(el) => {
                        if (el) {
                          audioRefs.current[call.id] = el;
                        } else {
                          delete audioRefs.current[call.id];
                        }
                      }}
                      src={getAbsoluteUrl(call.recording_url)}
                      onEnded={() => setPlayingId(null)}
                    />
                    <button
                      onClick={() => {
                        const audio = audioRefs.current[call.id];
                        if (!audio) return;
                        if (playingId === call.id) {
                          audio.pause();
                          setPlayingId(null);
                        } else {
                          if (playingId && audioRefs.current[playingId]) {
                            audioRefs.current[playingId]!.pause();
                          }
                          void audio.play().catch((error: unknown) => {
                            if (
                              error instanceof DOMException &&
                              error.name === "AbortError"
                            ) {
                              return;
                            }
                            console.error("Failed to play recording", error);
                          });
                          setPlayingId(call.id);
                        }
                      }}
                      title={playingId === call.id ? "Pause" : "Play recording"}
                      className="flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-primary/8 text-primary hover:bg-primary/15 transition-colors"
                    >
                      {playingId === call.id ? (
                        <HugeiconsIcon icon={PauseIcon} className="size-3 fill-current" />
                      ) : (
                        <HugeiconsIcon icon={PlayIcon} className="size-3 fill-current" />
                      )}
                      <span className="text-[10px] font-medium">
                        {playingId === call.id ? "Pause" : "Play"}
                      </span>
                    </button>
                  </>
                ) : (
                  <span
                    title="No recording"
                    className="flex items-center gap-1 text-muted-foreground/40"
                  >
                    <HugeiconsIcon icon={MicOff01Icon} className="size-3.5" />
                    <span className="text-[10px]">None</span>
                  </span>
                )}
              </div>

              <Link
                href={`/dashboard/${workspace_slug}/calls/${call.id}`}
                className="flex items-center gap-1.5 justify-end w-20 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring rounded-sm"
              >
                <div
                  className={`size-1.5 rounded-full ${call.status === "completed" ? "bg-success" : "bg-amber-400"}`}
                />
                <span className="text-xs text-foreground capitalize">{call.status}</span>
              </Link>

              <Link
                href={`/dashboard/${workspace_slug}/calls/${call.id}`}
                aria-label="Open call details"
                className="justify-self-end rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <HugeiconsIcon
                  icon={ArrowRight01Icon}
                  className="size-4 text-muted-foreground/40 group-hover:text-muted-foreground transition-colors"
                />
              </Link>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
