"use client";

import { useState } from "react";
import { type Agent } from "@/lib/api/client";
import ManualTestView from "@/components/agent/manual-test-view";
import AutoTestView from "@/components/agent/auto-test-view";
import { HugeiconsIcon } from "@hugeicons/react";
import { KeyboardIcon, VoiceIcon, Settings02Icon, Clock01Icon } from "@hugeicons/core-free-icons";

type PrimaryTab = "manual" | "auto";
type ManualSubTab = "voice" | "text";
type AutoSubTab = "configs" | "runs";

interface TestPanelProps {
  agentId: string;
  agent: Agent | null;
  onGoToConfig?: () => void;
}

export default function TestPanel({ agentId, agent, onGoToConfig }: TestPanelProps) {
  const [primaryTab, setPrimaryTab] = useState<PrimaryTab>("manual");
  const [manualSubTab, setManualSubTab] = useState<ManualSubTab>("voice");
  const [autoSubTab, setAutoSubTab] = useState<AutoSubTab>("configs");

  const header = (
    <header className="z-30 flex items-center justify-between h-10 px-4 border-b border-border bg-background/80 backdrop-blur-xl sticky top-0">
      <div className="flex items-center gap-3">
        {/* Compact Mode Selector */}
        <nav className="flex items-center gap-0.5 p-0.5 rounded-lg bg-muted/40 border border-border/40">
          <button
            onClick={() => setPrimaryTab("manual")}
            className={`px-3 py-1 rounded-md text-[10px] font-bold uppercase tracking-tight transition-all ${
              primaryTab === "manual"
                ? "bg-background text-foreground shadow-sm ring-1 ring-border/20"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            Manual
          </button>
          <button
            onClick={() => setPrimaryTab("auto")}
            className={`px-3 py-1 rounded-md text-[10px] font-bold uppercase tracking-tight transition-all ${
              primaryTab === "auto"
                ? "bg-background text-foreground shadow-sm ring-1 ring-border/20"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            Auto
          </button>
        </nav>

        <div className="h-4 w-px bg-border/40 mx-1" />

        {/* Level 2 Navigation (Single Row) */}
        {primaryTab === "manual" ? (
          <div className="flex items-center gap-1">
            <button
              onClick={() => setManualSubTab("voice")}
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md transition-all ${
                manualSubTab === "voice"
                  ? "text-primary bg-primary/10 font-bold"
                  : "text-muted-foreground hover:text-foreground hover:bg-accent/40"
              }`}
            >
              <HugeiconsIcon icon={VoiceIcon} className="size-3" />
              <span className="text-[10px] font-semibold">Voice</span>
            </button>
            <button
              onClick={() => setManualSubTab("text")}
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md transition-all ${
                manualSubTab === "text"
                  ? "text-primary bg-primary/10 font-bold"
                  : "text-muted-foreground hover:text-foreground hover:bg-accent/40"
              }`}
            >
              <HugeiconsIcon icon={KeyboardIcon} className="size-3" />
              <span className="text-[10px] font-semibold">Text</span>
            </button>
          </div>
        ) : (
          <div className="flex items-center gap-1">
            <button
              onClick={() => setAutoSubTab("configs")}
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md transition-all ${
                autoSubTab === "configs"
                  ? "text-primary bg-primary/10 font-bold"
                  : "text-muted-foreground hover:text-foreground hover:bg-accent/40"
              }`}
            >
              <HugeiconsIcon icon={Settings02Icon} className="size-3" />
              <span className="text-[10px] font-semibold">Config</span>
            </button>
            <button
              onClick={() => setAutoSubTab("runs")}
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md transition-all ${
                autoSubTab === "runs"
                  ? "text-primary bg-primary/10 font-bold"
                  : "text-muted-foreground hover:text-foreground hover:bg-accent/40"
              }`}
            >
              <HugeiconsIcon icon={Clock01Icon} className="size-3" />
              <span className="text-[10px] font-semibold">History</span>
            </button>
          </div>
        )}
      </div>
    </header>
  );

  return (
    <div className="relative flex h-full flex-col overflow-hidden bg-background/20">
      {header}

      <div className="relative flex-1 overflow-hidden">
        {primaryTab === "manual" ? (
          <ManualTestView
            agentId={agentId}
            agent={agent}
            activeMode={manualSubTab}
            onModeChange={setManualSubTab}
            onGoToConfig={onGoToConfig}
          />
        ) : (
          <AutoTestView
            agentId={agentId}
            agent={agent}
            activeTab={autoSubTab}
            onTabChange={setAutoSubTab}
          />
        )}
      </div>
    </div>
  );
}
