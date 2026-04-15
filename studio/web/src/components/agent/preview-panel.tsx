"use client";

import React from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  HeadsetIcon,
  ConnectIcon,
  Settings05Icon,
  WorkflowSquare10Icon,
} from "@hugeicons/core-free-icons";
import { cn } from "@/lib/utils";

export type PreviewTab = "flow" | "config" | "credentials" | "test";

interface PreviewPanelProps {
  activeTab: PreviewTab;
  setActiveTab: (tab: PreviewTab) => void;
  credentialCount: number;
  headerActions?: React.ReactNode;
  children: React.ReactNode;
}

const tabs: {
  key: PreviewTab;
  label: string;
  icon: readonly (readonly [string, { readonly [key: string]: string | number }])[];
}[] = [
  { key: "config", label: "Config", icon: Settings05Icon },
  { key: "flow", label: "Flow", icon: WorkflowSquare10Icon },
  { key: "credentials", label: "Connections", icon: ConnectIcon },
  { key: "test", label: "Test", icon: HeadsetIcon },
];

export default function PreviewPanel({
  activeTab,
  setActiveTab,
  credentialCount,
  headerActions,
  children,
}: PreviewPanelProps) {
  return (
    <div className="relative flex flex-1 flex-col bg-background/50 min-w-0 overflow-hidden">
      {/* Premium Tab bar */}
      <div className="flex h-14 items-center justify-between gap-4 border-b border-border/60 bg-accent/20 px-4 shrink-0 shadow-[0_1px_10px_rgba(0,0,0,0.02)]">
        <div className="flex items-center gap-1.5 p-1 rounded-xl bg-background/40 border border-border/40 backdrop-blur-sm">
          {tabs.map((tab) => (
            <button
              key={tab.key}
              onClick={() => setActiveTab(tab.key)}
              className={cn(
                "flex items-center gap-2 px-3 py-1.5 rounded-lg text-[10px] font-bold transition-all duration-300 select-none outline-none group/tab",
                activeTab === tab.key
                  ? "bg-background text-foreground shadow-sm ring-1 ring-border/20"
                  : "text-muted-foreground hover:text-foreground hover:bg-accent/40"
              )}
            >
              <HugeiconsIcon
                icon={tab.icon}
                className={cn(
                  "size-3.5 transition-transform group-hover/tab:scale-110",
                  activeTab === tab.key ? "text-primary" : "text-muted-foreground"
                )}
              />
              <span className="hidden sm:inline">{tab.label}</span>
              {tab.key === "credentials" && credentialCount > 0 && (
                <span
                  className={cn(
                    "flex items-center justify-center min-w-5 h-4 rounded-full text-[10px] font-black tracking-tight",
                    activeTab === tab.key
                      ? "bg-primary text-primary-foreground"
                      : "bg-muted text-muted-foreground"
                  )}
                >
                  {credentialCount}
                </span>
              )}
            </button>
          ))}
        </div>

        {headerActions && (
          <div className="flex items-center shrink-0 pr-1 animate-in fade-in slide-in-from-right-3 duration-500">
            {headerActions}
          </div>
        )}
      </div>

      <div className="flex-1 overflow-y-auto relative">
        {/* Subtle background gradient to make the content feel layered */}
        <div className="absolute inset-0 bg-linear-to-br from-transparent via-accent/5 to-transparent pointer-events-none" />
        <div className="relative z-10 h-full">{children}</div>
      </div>
    </div>
  );
}
