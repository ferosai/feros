"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import { Alert01Icon, PauseIcon, PlayIcon } from "@hugeicons/core-free-icons";
import { useEffect, useMemo, useRef, useState } from "react";
import WaveSurfer from "wavesurfer.js";
import { Button } from "@/components/ui/button";

import { formatDuration } from "./types";

function isLikelyOpusUrl(url: string): boolean {
  return /\.opus(\?|$)/i.test(url) || /audio_format=opus/i.test(url);
}

function supportsOpusPlayback(): boolean {
  const audio = document.createElement("audio");
  return audio.canPlayType('audio/ogg; codecs="opus"') !== "";
}

export function CallWaveform({
  recordingUrl,
  fallbackDurationSec,
  onDurationReady,
  onTimeUpdate,
}: {
  recordingUrl: string | null;
  fallbackDurationSec?: number | null;
  onDurationReady?: (durationSec: number) => void;
  onTimeUpdate: (timeSec: number) => void;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const waveRef = useRef<WaveSurfer | null>(null);

  const [durationSec, setDurationSec] = useState<number>(0);
  const [currentSec, setCurrentSec] = useState<number>(0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const compatibilityError = useMemo(() => {
    if (!recordingUrl) return null;
    if (isLikelyOpusUrl(recordingUrl) && !supportsOpusPlayback()) {
      return "This browser does not support OGG/Opus playback.";
    }
    return null;
  }, [recordingUrl]);

  useEffect(() => {
    if (!recordingUrl || compatibilityError || !containerRef.current) {
      return;
    }

    const ws = WaveSurfer.create({
      container: containerRef.current,
      url: recordingUrl,
      backend: "WebAudio",
      waveColor: "#9ca3af",
      progressColor: "#0f766e",
      cursorColor: "#0f172a",
      cursorWidth: 1,
      height: 92,
      barWidth: 2,
      barGap: 1,
      barRadius: 2,
      dragToSeek: true,
    });
    waveRef.current = ws;

    ws.on("ready", () => {
      const duration = ws.getDuration();
      const normalizedDuration = Number.isFinite(duration) ? duration : 0;
      setDurationSec(normalizedDuration);
      onDurationReady?.(normalizedDuration);
    });

    ws.on("timeupdate", (t) => {
      const time = Number.isFinite(t) ? t : 0;
      setCurrentSec(time);
      onTimeUpdate(time);
    });

    ws.on("play", () => setIsPlaying(true));
    ws.on("pause", () => setIsPlaying(false));
    ws.on("error", () => {
      setLoadError("Failed to load recording.");
      setIsPlaying(false);
    });

    return () => {
      onTimeUpdate(0);
      onDurationReady?.(0);
      ws.destroy();
      waveRef.current = null;
    };
  }, [recordingUrl, compatibilityError, onTimeUpdate, onDurationReady]);

  const togglePlay = () => {
    if (!waveRef.current) return;
    void waveRef.current.playPause();
  };

  const displayedDuration =
    durationSec > 0
      ? durationSec
      : typeof fallbackDurationSec === "number"
        ? fallbackDurationSec
        : 0;

  return (
    <section className="flat-card p-5 space-y-4">
      <div className="flex items-center justify-between gap-4">
        <h3 className="text-sm font-semibold text-foreground">Audio</h3>
        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <span className="font-mono">{formatDuration(currentSec)}</span>
          <span>/</span>
          <span className="font-mono">{formatDuration(displayedDuration)}</span>
        </div>
      </div>

      {!recordingUrl ? (
        <Unavailable text="No recording is available for this call." />
      ) : compatibilityError ? (
        <Unavailable text={compatibilityError} />
      ) : loadError ? (
        <Unavailable text={loadError} />
      ) : (
        <>
          <div className="rounded-lg border border-border/40 bg-secondary/20 p-3">
            <div ref={containerRef} />
          </div>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-8 gap-1.5"
              onClick={togglePlay}
            >
              {isPlaying ? (
                <HugeiconsIcon icon={PauseIcon} className="size-3.5" />
              ) : (
                <HugeiconsIcon icon={PlayIcon} className="size-3.5" />
              )}
              {isPlaying ? "Pause" : "Play"}
            </Button>
            <p className="text-xs text-muted-foreground">Click anywhere on the waveform to seek.</p>
          </div>
        </>
      )}
    </section>
  );
}

function Unavailable({ text }: { text: string }) {
  return (
    <div className="rounded-lg border border-dashed border-border/60 p-6 text-center">
      <HugeiconsIcon icon={Alert01Icon} className="mx-auto size-6 text-muted-foreground/60" />
      <p className="mt-2 text-sm text-muted-foreground">{text}</p>
    </div>
  );
}
