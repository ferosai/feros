"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowDown01Icon,
  CheckmarkCircle02Icon,
  FilterMailCircleIcon,
  Search01Icon,
  VoiceIcon,
} from "@hugeicons/core-free-icons";
import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { parseAsNativeArrayOf, parseAsString, useQueryState } from "nuqs";
import { api, type Agent, type CallLog } from "@/lib/api/client";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { CallLogTable } from "@/components/calls/call-log-table";
import { PageHeader } from "@/components/ui/page-header";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

const AGENT_MENU_LIMIT = 10;
const CALLS_PAGE_LIMIT = 50;

import { Suspense } from "react";

function CallsPageContent() {
  const router = useRouter();
  const [calls, setCalls] = useState<CallLog[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedAgentIds, setSelectedAgentIds] = useQueryState(
    "agent_ids",
    parseAsNativeArrayOf(parseAsString)
  );
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  const [agentSearchInput, setAgentSearchInput] = useState("");
  const [agentSearchQuery, setAgentSearchQuery] = useState("");
  const [agentOptions, setAgentOptions] = useState<Agent[]>([]);
  const [agentOptionsSkip, setAgentOptionsSkip] = useState(0);
  const [agentOptionsTotal, setAgentOptionsTotal] = useState(0);
  const [agentOptionsLoading, setAgentOptionsLoading] = useState(false);
  const [agentNameById, setAgentNameById] = useState<Record<string, string>>({});
  const [callsSkip, setCallsSkip] = useState(0);
  const [callsTotal, setCallsTotal] = useState(0);

  useEffect(() => {
    const timeout = window.setTimeout(() => {
      setAgentSearchQuery(agentSearchInput.trim());
      setAgentOptionsSkip(0);
    }, 250);

    return () => window.clearTimeout(timeout);
  }, [agentSearchInput]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);

    api.calls
      .list(selectedAgentIds, callsSkip, CALLS_PAGE_LIMIT)
      .then((data) => {
        if (cancelled) return;
        setCalls(data.calls);
        setCallsTotal(data.total);
        setAgentNameById((prev) => {
          const next = { ...prev };
          for (const call of data.calls) {
            if (call.agent_name) {
              next[call.agent_id] = call.agent_name;
            }
          }
          return next;
        });
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [callsSkip, selectedAgentIds]);

  useEffect(() => {
    if (!agentMenuOpen) return;
    let cancelled = false;
    const loadAgentOptions = async () => {
      setAgentOptionsLoading(true);
      try {
        const data = await api.agents.list(
          agentOptionsSkip,
          AGENT_MENU_LIMIT,
          agentSearchQuery || undefined
        );
        if (cancelled) return;
        setAgentOptions(data.agents);
        setAgentOptionsTotal(data.total);
        setAgentNameById((prev) => {
          const next = { ...prev };
          for (const agent of data.agents) {
            next[agent.id] = agent.name;
          }
          return next;
        });
      } catch {
        if (cancelled) return;
        setAgentOptions([]);
        setAgentOptionsTotal(0);
      } finally {
        if (!cancelled) setAgentOptionsLoading(false);
      }
    };

    void loadAgentOptions();

    return () => {
      cancelled = true;
    };
  }, [agentMenuOpen, agentOptionsSkip, agentSearchQuery]);

  const toggleAgentSelection = (agent: Agent) => {
    setAgentNameById((prev) => ({ ...prev, [agent.id]: agent.name }));
    setCallsSkip(0);
    void setSelectedAgentIds((prev) =>
      prev.includes(agent.id) ? prev.filter((id) => id !== agent.id) : [...prev, agent.id]
    );
    setLoading(true);
  };

  const canPrevAgentPage = agentOptionsSkip > 0;
  const canNextAgentPage = agentOptionsSkip + AGENT_MENU_LIMIT < agentOptionsTotal;
  const agentPage = Math.floor(agentOptionsSkip / AGENT_MENU_LIMIT) + 1;
  const agentPageCount = Math.max(1, Math.ceil(agentOptionsTotal / AGENT_MENU_LIMIT));
  const canPrevCallsPage = callsSkip > 0;
  const canNextCallsPage = callsSkip + calls.length < callsTotal;
  const callsPage = Math.floor(callsSkip / CALLS_PAGE_LIMIT) + 1;
  const callsPageCount = Math.max(1, Math.ceil(callsTotal / CALLS_PAGE_LIMIT));

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <PageHeader icon={VoiceIcon} title="Calls" description="View and manage call history" />
        <div className="flex gap-3 items-center">
          <div className="relative group hidden md:block">
            <HugeiconsIcon
              icon={Search01Icon}
              className="absolute left-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground group-focus-within:text-primary transition-colors"
            />
            <input
              type="text"
              placeholder="Search calls..."
              className="h-9 w-48 rounded-lg bg-secondary pl-9 pr-3 text-xs focus:bg-card focus:outline-none focus:ring-2 focus:ring-ring placeholder:text-muted-foreground transition-all"
            />
          </div>
          <Popover open={agentMenuOpen} onOpenChange={setAgentMenuOpen}>
            <PopoverTrigger asChild>
              <Button
                variant="outline"
                size="sm"
                role="combobox"
                aria-expanded={agentMenuOpen}
                className="h-9 rounded-lg px-3 text-xs font-medium min-w-[260px] justify-between gap-2"
              >
                <span className="flex items-center gap-2 min-w-0">
                  <HugeiconsIcon
                    icon={FilterMailCircleIcon}
                    className={`size-4 shrink-0 ${selectedAgentIds.length > 0 ? "text-primary" : "text-muted-foreground"}`}
                  />
                  {selectedAgentIds.length === 0 ? (
                    <span className="truncate text-muted-foreground">All Agents</span>
                  ) : (
                    <span className="flex items-center gap-1.5 min-w-0">
                      <Badge variant="secondary" className="max-w-[150px] truncate">
                        {agentNameById[selectedAgentIds[0]] ?? "Unknown"}
                      </Badge>
                      {selectedAgentIds.length > 1 ? (
                        <Badge variant="outline">+{selectedAgentIds.length - 1}</Badge>
                      ) : null}
                    </span>
                  )}
                </span>
                <HugeiconsIcon
                  icon={ArrowDown01Icon}
                  className="size-3.5 text-muted-foreground shrink-0"
                />
              </Button>
            </PopoverTrigger>
            <PopoverContent className="w-[340px] p-0" align="end">
              <Command shouldFilter={false}>
                <CommandInput
                  placeholder="Filter by agent name..."
                  value={agentSearchInput}
                  onValueChange={setAgentSearchInput}
                />
                <CommandList>
                  {agentOptionsLoading ? (
                    <div className="space-y-px p-1">
                      {Array.from({ length: AGENT_MENU_LIMIT }).map((_, idx) => (
                        <div key={idx} className="h-9 rounded-sm bg-secondary/30 animate-pulse" />
                      ))}
                    </div>
                  ) : (
                    <>
                      <CommandEmpty>No agents found.</CommandEmpty>
                      <CommandGroup>
                        {agentOptions.map((agent) => {
                          const isSelected = selectedAgentIds.includes(agent.id);
                          return (
                            <CommandItem
                              key={agent.id}
                              value={agent.name}
                              onSelect={() => toggleAgentSelection(agent)}
                              className="justify-between"
                            >
                              <span className={`truncate ${isSelected ? "font-semibold" : ""}`}>
                                {agent.name}
                              </span>
                              {isSelected ? (
                                <HugeiconsIcon
                                  icon={CheckmarkCircle02Icon}
                                  className="size-4 text-primary shrink-0"
                                />
                              ) : null}
                            </CommandItem>
                          );
                        })}
                      </CommandGroup>
                    </>
                  )}
                </CommandList>
                <CommandSeparator />
                <div className="grid grid-cols-3 items-center p-2 text-[10px] text-muted-foreground">
                  <div className="justify-self-start">
                    {agentPageCount > 1 ? (
                      <span>
                        Page {agentPage}/{agentPageCount}
                      </span>
                    ) : null}
                  </div>
                  <div className="justify-self-center">
                    {selectedAgentIds.length > 0 ? (
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        className="h-7 px-2 text-[10px]"
                        onClick={() => {
                          setLoading(true);
                          setCallsSkip(0);
                          void setSelectedAgentIds([]);
                        }}
                      >
                        Clear All
                      </Button>
                    ) : null}
                  </div>
                  <div className="flex gap-1 justify-self-end">
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-[10px]"
                      disabled={!canPrevAgentPage || agentOptionsLoading}
                      onClick={() =>
                        setAgentOptionsSkip((prev) => Math.max(0, prev - AGENT_MENU_LIMIT))
                      }
                    >
                      Prev
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-[10px]"
                      disabled={!canNextAgentPage || agentOptionsLoading}
                      onClick={() => setAgentOptionsSkip((prev) => prev + AGENT_MENU_LIMIT)}
                    >
                      Next
                    </Button>
                  </div>
                </div>
              </Command>
            </PopoverContent>
          </Popover>
        </div>
      </div>

      <CallLogTable
        calls={calls}
        loading={loading}
        agentNameById={agentNameById}
        onOpenCall={(callId) => router.push(`/dashboard/calls/${callId}`)}
        showColumnHeader
        loadingRows={5}
      />

      {calls.length > 0 && (
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span>
            {callsTotal} calls
            {callsPageCount > 1 ? ` · Page ${callsPage}/${callsPageCount}` : ""}
          </span>
          <div className="flex gap-2">
            <Button
              variant="secondary"
              disabled={!canPrevCallsPage || loading}
              size="sm"
              className="text-xs h-8"
              onClick={() => setCallsSkip((prev) => Math.max(0, prev - CALLS_PAGE_LIMIT))}
            >
              Previous
            </Button>
            <Button
              variant="secondary"
              disabled={!canNextCallsPage || loading}
              size="sm"
              className="text-xs h-8 text-primary"
              onClick={() => setCallsSkip((prev) => prev + CALLS_PAGE_LIMIT)}
            >
              Next
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

export default function CallsPage() {
  return (
    <Suspense
      fallback={
        <div className="flex h-[400px] items-center justify-center p-4">Loading calls...</div>
      }
    >
      <CallsPageContent />
    </Suspense>
  );
}
