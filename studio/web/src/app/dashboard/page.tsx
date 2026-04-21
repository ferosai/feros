"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  AiAudioIcon,
  BookOpen01Icon,
  CallInternal02Icon,
  Call02Icon,
  OneCircleIcon,
  Robot01Icon,
  ThreeCircleIcon,
  TwoCircleIcon,
  SparklesIcon,
} from "@hugeicons/core-free-icons";
import { useEffect, useState } from "react";
import { api, type Agent, type CallLog } from "@/lib/api/client";
import { Button } from "@/components/ui/button";
import { CallLogTable } from "@/components/calls/call-log-table";
import AgentListItemCard from "@/components/agent/agent-list-item-card";
import { PageHeader } from "@/components/ui/page-header";
import { DOCS_URL } from "@/lib/constants";
import Link from "next/link";

const quickActions = [
  {
    stepIcon: OneCircleIcon,
    bgIcon: AiAudioIcon,
    title: "Setup Models",
    description: "Set up Builder and Voice Agent LLM, STT, and TTS models in Settings",
    href: "/dashboard/settings",
  },
  {
    stepIcon: TwoCircleIcon,
    bgIcon: Robot01Icon,
    title: "Build & Test Agent",
    description:
      "Build your agent with Agent Builder, then run test calls to verify prompts, tools, and voice behavior",
    href: "/dashboard/agents/new",
  },
  {
    stepIcon: ThreeCircleIcon,
    bgIcon: CallInternal02Icon,
    title: "Config Phone Numbers",
    description:
      "After your agent is ready, connect an external phone number so it can handle real calls",
    href: "/dashboard/phone-numbers",
  },
];

export default function DashboardPage() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [recentCalls, setRecentCalls] = useState<CallLog[]>([]);
  const [agentsLoading, setAgentsLoading] = useState(true);
  const [callsLoading, setCallsLoading] = useState(true);

  useEffect(() => {
    api.agents
      .list(0, 5)
      .then((data) => setAgents(data.agents))
      .catch(() => {})
      .finally(() => setAgentsLoading(false));

    api.calls
      .list(undefined, 0, 5)
      .then((data) => setRecentCalls(data.calls))
      .catch(() => {})
      .finally(() => setCallsLoading(false));
  }, []);

  return (
    <div className="space-y-10">
      {/* Quick actions — flat bordered cards */}
      <div className="space-y-6">
        <div className="flex items-center justify-between gap-4">
          <PageHeader icon={SparklesIcon} title="Getting Started" />
          <Link
            href={DOCS_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-primary transition-colors shrink-0"
          >
            <HugeiconsIcon icon={BookOpen01Icon} className="size-4" />
            Read the Docs
          </Link>
        </div>
        <div className="grid gap-4 sm:grid-cols-3">
          {quickActions.map((action) => (
            <Link key={action.title} href={action.href} className="block h-full">
              <div className="relative overflow-hidden ring-1 ring-foreground/10 hover:ring-primary/20 hover:bg-primary/5 hover:shadow rounded-lg p-5 cursor-pointer group h-full flex">
                <HugeiconsIcon
                  icon={action.bgIcon}
                  className="absolute -bottom-6 right-0 size-24 text-foreground/5 pointer-events-none transition-transform duration-500 ease-out group-hover:scale-125"
                />
                <div className="relative flex items-start gap-2.5">
                  <HugeiconsIcon
                    icon={action.stepIcon}
                    className="size-6 text-primary mt-0.5 shrink-0"
                  />
                  <div>
                    <p className="text-sm font-semibold text-foreground mb-1">{action.title}</p>
                    <p className="text-xs text-muted-foreground leading-relaxed">
                      {action.description}
                    </p>
                  </div>
                </div>
              </div>
            </Link>
          ))}
        </div>
      </div>

      {/* Agents section */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-semibold text-foreground">Agents</h2>
          <Link
            href="/dashboard/agents"
            className="text-sm text-muted-foreground hover:text-primary transition-colors"
          >
            View all
          </Link>
        </div>

        {agentsLoading ? (
          <div className="space-y-2">
            {[1, 2].map((i) => (
              <div key={i} className="h-16 rounded-xl bg-secondary animate-pulse" />
            ))}
          </div>
        ) : agents.length === 0 ? (
          <div className="flat-card p-10 text-center">
            <div className="inline-flex items-center justify-center size-12 rounded-xl bg-secondary mb-4">
              <HugeiconsIcon icon={Robot01Icon} className="size-5 text-muted-foreground" />
            </div>
            <p className="text-sm font-medium text-foreground mb-1">No agents yet</p>
            <p className="text-sm text-muted-foreground mb-5 max-w-[260px] mx-auto">
              Create your first voice agent to start handling calls automatically.
            </p>
            <Link href="/dashboard/agents/new">
              <Button size="sm" className="h-8 rounded-lg px-4 text-xs font-medium gap-1.5">
                <HugeiconsIcon icon={Add01Icon} className="size-3.5" /> Create agent
              </Button>
            </Link>
          </div>
        ) : (
          <div className="space-y-2">
            {agents.map((agent) => (
              <AgentListItemCard
                key={agent.id}
                agent={agent}
                href={`/dashboard/agents/${agent.id}`}
              />
            ))}
          </div>
        )}
      </section>

      {/* Recent calls */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-semibold text-foreground">Recent calls</h2>
          <Link
            href="/dashboard/calls"
            className="text-sm text-muted-foreground hover:text-primary transition-colors"
          >
            View all
          </Link>
        </div>

        {callsLoading ? (
          <div className="space-y-2">
            {[1, 2].map((i) => (
              <div key={i} className="h-16 rounded-xl bg-secondary animate-pulse" />
            ))}
          </div>
        ) : recentCalls.length === 0 ? (
          <div className="flat-card p-10 text-center">
            <div className="inline-flex items-center justify-center size-12 rounded-xl bg-secondary mb-4">
              <HugeiconsIcon icon={Call02Icon} className="size-5 text-muted-foreground" />
            </div>
            <p className="text-sm font-medium text-foreground mb-1">No calls yet</p>
            <p className="text-sm text-muted-foreground max-w-[260px] mx-auto">
              Call logs will show up here once your agents start handling traffic.
            </p>
          </div>
        ) : (
          <CallLogTable
            calls={recentCalls}
            loading={false}
            showColumnHeader={false}
            loadingRows={2}
          />
        )}
      </section>
    </div>
  );
}
