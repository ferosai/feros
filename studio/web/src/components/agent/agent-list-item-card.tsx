"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowRight01Icon,
  LanguageCircleIcon,
  Robot01Icon,
  TimeZoneIcon,
} from "@hugeicons/core-free-icons";
import Link from "next/link";

import type { Agent } from "@/lib/api/client";

type AgentListItemCardProps = {
  agent: Agent;
  href: string;
  className?: string;
};

function getLanguageFromConfig(config: Agent["current_config"]): string | null {
  if (!config || typeof config !== "object") return null;
  const language =
    "language" in config && typeof config.language === "string"
      ? config.language
      : "identity" in config &&
          config.identity &&
          typeof config.identity === "object" &&
          "language" in config.identity &&
          typeof config.identity.language === "string"
        ? config.identity.language
        : null;
  return language?.trim() || null;
}

function getTimezoneFromConfig(config: Agent["current_config"]): string | null {
  if (!config || typeof config !== "object") return null;
  const timezone =
    "timezone" in config && typeof config.timezone === "string" ? config.timezone : null;
  return timezone?.trim() || null;
}

function formatLanguageLabel(rawLanguage: string): string {
  const normalized = rawLanguage.replace("_", "-").trim();
  if (!normalized) return rawLanguage;
  const baseCode = normalized.split("-")[0].toLowerCase();
  try {
    const display = new Intl.DisplayNames(["en"], { type: "language" }).of(baseCode);
    if (display) return display;
  } catch {
    // Fallback below
  }
  return baseCode.toUpperCase();
}

function VersionBadge({
  label,
  version,
  tone,
}: {
  label: "current" | "active";
  version: number;
  tone: "neutral" | "accent";
}) {
  const isAccent = tone === "accent";
  return (
    <span
      className={`inline-flex h-6 items-center overflow-hidden rounded-md border text-[10px] font-semibold uppercase tracking-wide ${
        isAccent ? "border-primary/50 text-primary" : "border-border text-foreground"
      }`}
    >
      <span className="px-2">{label}</span>
      <span className={`border-l px-2 ${isAccent ? "border-primary/50" : "border-border"}`}>
        v{version}
      </span>
    </span>
  );
}

export default function AgentListItemCard({ agent, href, className }: AgentListItemCardProps) {
  const description = agent.description?.trim() || "";
  const currentVersion = agent.version_count > 0 ? agent.version_count : null;
  const activeVersion = agent.active_version;

  const showCurrent =
    currentVersion !== null && (activeVersion === null || activeVersion !== currentVersion);
  const showActive = activeVersion !== null;

  const rawLanguage = getLanguageFromConfig(agent.current_config);
  const timezone = getTimezoneFromConfig(agent.current_config);
  const language = rawLanguage ? formatLanguageLabel(rawLanguage) : null;

  const hasExtraDetails = Boolean(description || language || timezone);

  return (
    <Link href={href} className="block">
      <div
        className={`group flex items-center justify-between gap-4 rounded-xl border border-border px-4 py-3 hover:bg-secondary/50 transition-colors ${
          className ?? ""
        }`}
      >
        <div className={`min-w-0 flex gap-3 ${hasExtraDetails ? "items-start" : "items-center"}`}>
          <div className="size-9 shrink-0 rounded-lg bg-secondary text-muted-foreground group-hover:bg-primary/10 group-hover:text-primary transition-colors flex items-center justify-center">
            <HugeiconsIcon icon={Robot01Icon} className="size-4" />
          </div>
          <div className="min-w-0">
            <p className="truncate text-xs font-medium text-foreground">{agent.name}</p>
            {description ? (
              <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>
            ) : null}
            {language || timezone ? (
              <div className="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
                {language ? (
                  <span className="inline-flex items-center gap-1.5">
                    <HugeiconsIcon icon={LanguageCircleIcon} className="size-3.5" />
                    {language}
                  </span>
                ) : null}
                {timezone ? (
                  <span className="inline-flex items-center gap-1.5">
                    <HugeiconsIcon icon={TimeZoneIcon} className="size-3.5" />
                    {timezone}
                  </span>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-3">
          {showCurrent || showActive ? (
            <div className="flex items-center gap-1.5">
              {showCurrent && currentVersion !== null ? (
                <VersionBadge label="current" version={currentVersion} tone="neutral" />
              ) : null}
              {showActive && activeVersion !== null ? (
                <VersionBadge label="active" version={activeVersion} tone="accent" />
              ) : null}
            </div>
          ) : null}
          <HugeiconsIcon
            icon={ArrowRight01Icon}
            className="size-4 text-muted-foreground/40 group-hover:text-muted-foreground transition-colors"
          />
        </div>
      </div>
    </Link>
  );
}
