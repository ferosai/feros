"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowLeft01Icon,
  Robot01Icon,
  CheckmarkCircle02Icon,
  CodeIcon,
  Copy01Icon,
  Delete02Icon,
  Download01Icon,
  GitBranchIcon,
  Key02Icon,
  PencilEdit01Icon,
  Rocket01Icon,
  MoreHorizontalIcon,
  Clock01Icon,
  LeftToRightListBulletIcon,
  Settings03Icon,
} from "@hugeicons/core-free-icons";
import { useState, useRef, useEffect, useCallback, use } from "react";
import Link from "next/link";
import { useRouter, usePathname, useSearchParams } from "next/navigation";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { Skeleton } from "@/components/ui/skeleton";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

import {
  api,
  type Agent,
  type AgentVersion,
  type ActionCard,
  type Credential,
  type ImportedConnection,
  type OAuthApp,
  type IntegrationSummary,
} from "@/lib/api/client";

import ChatPanel, { type ChatMessage } from "@/components/agent/chat-panel";
import PreviewPanel, { type PreviewTab } from "@/components/agent/preview-panel";
import TestPanel from "@/components/agent/test-panel";
import AgentConfigEditor from "@/components/agent/agent-config-editor";
import {
  IntegrationConnectionDialog,
  OAuthAppRegistrationDialog,
} from "@/components/integrations/connection-dialogs";
import { IntegrationIcon } from "@/components/ui/integration-icon";
import { renderMermaid } from "beautiful-mermaid";
import { TransformWrapper, TransformComponent } from "react-zoom-pan-pinch";

// ── Page ─────────────────────────────────────────────────────────

function normalizeMermaidSource(source: string): string {
  let text = source.trim();
  if (text.startsWith("```")) {
    text = text
      .replace(/^```(?:mermaid)?\s*/i, "")
      .replace(/\s*```$/, "")
      .trim();
  }
  const lines = text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines[0]?.toLowerCase() === "mermaid") {
    lines.shift();
  }
  return lines.join("\n");
}

async function renderFlowDiagram(source: string): Promise<string> {
  const baseTheme = {
    font: "Outfit",
    bg: "var(--background)",
    fg: "var(--foreground)",
    muted: "var(--muted-foreground)",
    line: "#64748b", // Distinct slate color for connection lines
    border: "var(--border)",
    surface: "var(--background)",
    accent: "var(--primary)",
    padding: 32,
  } as const;

  try {
    return await renderMermaid(source, {
      ...baseTheme,
      nodeSpacing: 24,
      layerSpacing: 32,
    });
  } catch {
    return await renderMermaid(source, baseTheme);
  }
}

type FlowUpdateState = {
  mode: "create" | "update";
};

import { Suspense } from "react";

function AgentDetailPageContent({ params }: { params: Promise<{ id: string }> }) {
  const router = useRouter();
  const searchParams = useSearchParams();
  const pathname = usePathname();
  const { id } = use(params);
  const [agent, setAgent] = useState<Agent | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [activeTab, setActiveTabInternal] = useState<PreviewTab>(
    (searchParams.get("tab") as PreviewTab) || "config"
  );

  const setActiveTab = useCallback(
    (tab: PreviewTab) => {
      setActiveTabInternal(tab);
      const newParams = new URLSearchParams(searchParams.toString());
      newParams.set("tab", tab);
      router.replace(`${pathname}?${newParams.toString()}`, { scroll: false });
    },
    [pathname, router, searchParams]
  );
  const [mermaidDiagram, setMermaidDiagram] = useState<string | null>(null);
  const [mermaidRenderVersion, setMermaidRenderVersion] = useState(0);
  const [flowUpdateState, setFlowUpdateState] = useState<FlowUpdateState | null>(null);
  const mermaidRef = useRef<HTMLDivElement>(null);
  const pendingMermaidRenderRef = useRef(false);

  // Credentials state
  const [credentials, setCredentials] = useState<Credential[]>([]);
  const [importedConnections, setImportedConnections] = useState<ImportedConnection[]>([]);
  const [credLoading, setCredLoading] = useState(false);
  const credentialRequestSeqRef = useRef(0);
  const [oauthApps, setOAuthApps] = useState<OAuthApp[]>([]);

  const [recentlySavedSkill, setRecentlySavedSkill] = useState<string | null>(null);
  const [overrideDialogOpen, setOverrideDialogOpen] = useState(false);
  const [registerOAuthOpen, setRegisterOAuthOpen] = useState(false);
  const [selectedIntegration, setSelectedIntegration] = useState<IntegrationSummary | null>(null);
  const [selectedExisting, setSelectedExisting] = useState<Credential | null>(null);
  const [oauthEditReturnTarget, setOauthEditReturnTarget] = useState<"none" | "override">("none");

  // Diff state (from pipeline SSE)
  const [lastDiff, setLastDiff] = useState<string | null>(null);

  // Deploy state
  const [deployLoading, setDeployLoading] = useState(false);
  const [revertLoading, setRevertLoading] = useState(false);
  const [deployDialogOpen, setDeployDialogOpen] = useState(false);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionOptions, setVersionOptions] = useState<AgentVersion[]>([]);
  const [selectedDeployVersion, setSelectedDeployVersion] = useState<number | null>(null);
  const [deployPage, setDeployPage] = useState(0);
  const [selectedVersionConfig, setSelectedVersionConfig] = useState<AgentVersion["config"] | null>(
    null
  );
  const [renameDialogOpen, setRenameDialogOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [renameLoading, setRenameLoading] = useState(false);
  const renameInputRef = useRef<HTMLInputElement>(null);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteConfirmName, setDeleteConfirmName] = useState("");
  const [deleteLoading, setDeleteLoading] = useState(false);
  const pageSize = 10;

  // ── Data Loading ──────────────────────────────────────────────

  useEffect(() => {
    if (!renameDialogOpen) return;
    const timer = window.setTimeout(() => {
      renameInputRef.current?.focus();
      renameInputRef.current?.select();
    }, 0);
    return () => window.clearTimeout(timer);
  }, [renameDialogOpen]);

  const loadCredentials = useCallback(async () => {
    const requestSeq = credentialRequestSeqRef.current + 1;
    credentialRequestSeqRef.current = requestSeq;
    setCredLoading(true);
    try {
      const res = await api.credentials.list(id);
      if (credentialRequestSeqRef.current === requestSeq) {
        setCredentials(res.credentials);
      }
    } catch {
      // Keep the current list on transient refresh failures.
    } finally {
      if (credentialRequestSeqRef.current === requestSeq) {
        setCredLoading(false);
      }
    }
  }, [id]);

  const loadVersions = useCallback(async () => {
    setVersionsLoading(true);
    try {
      const versions = await api.agents.versions(id);
      setVersionOptions(versions);
      if (versions.length > 0) {
        const latest = versions[0];
        setAgent((prev) => (prev ? { ...prev, current_config: latest.config } : prev));
      }
    } catch {
      // ignore
    } finally {
      setVersionsLoading(false);
    }
  }, [id]);

  useEffect(() => {
    const setWelcome = () => {
      setMessages([
        {
          id: "welcome",
          role: "assistant",
          parts: [
            {
              kind: "text",
              content:
                'Hi! I\'m your agent builder. Describe the voice agent you want to create and I\'ll set everything up.\n\nFor example:\n\n- "A restaurant booking assistant"\n- "A customer support agent for a SaaS product"\n- "An appointment scheduler that saves leads to Airtable"',
            },
          ],
        },
      ]);
    };

    api.agents
      .get(id)
      .then(setAgent)
      .catch(() => {});
    void loadVersions();
    void loadCredentials();
    void api.oauthApps
      .list()
      .then(setOAuthApps)
      .catch(() => {});
    // Reset stale state from previous agent
    setMermaidDiagram(null);
    setMermaidRenderVersion(0);
    setFlowUpdateState(null);
    pendingMermaidRenderRef.current = false;
    setImportedConnections([]);
    api.builder
      .getConversation(id)
      .then((conv) => {
        if (conv.messages.length > 0) {
          const loaded: ChatMessage[] = conv.messages.map((m) => ({
            id: m.id,
            role: m.role,
            parts: m.parts,
            actionCards: m.action_cards,
          }));
          setMessages(loaded);

          for (let i = conv.messages.length - 1; i >= 0; i--) {
            const msg = conv.messages[i];
            if (!msg) continue;
            if (msg.mermaid_diagram) {
              setMermaidDiagram(msg.mermaid_diagram);
            }
            if (
              msg.imported_connections &&
              msg.imported_connections.length > 0
            ) {
              setImportedConnections(msg.imported_connections);
            }
            if (
              msg.mermaid_diagram ||
              (msg.imported_connections && msg.imported_connections.length > 0)
            ) {
              break;
            }
          }
        } else {
          setWelcome();
        }
      })
      .catch(() => {
        setWelcome();
      });
  }, [id, loadCredentials, loadVersions]);

  // Render mermaid
  useEffect(() => {
    if (!mermaidDiagram || !mermaidRef.current || activeTab !== "flow") return;
    let cancelled = false;
    let finishFrame: number | null = null;
    (async () => {
      try {
        const normalizedDiagram = normalizeMermaidSource(mermaidDiagram);
        const svg = await renderFlowDiagram(normalizedDiagram);
        if (!cancelled && mermaidRef.current) {
          mermaidRef.current.innerHTML = svg;
        }
        finishFrame = window.requestAnimationFrame(() => {
          if (cancelled) return;
          pendingMermaidRenderRef.current = false;
          setFlowUpdateState(null);
        });
      } catch (error) {
        console.error("Beautiful Mermaid render failed", error);
        if (!cancelled && mermaidRef.current) {
          mermaidRef.current.innerHTML = `<pre class="text-xs text-muted-foreground p-4">${mermaidDiagram}</pre>`;
        }
        finishFrame = window.requestAnimationFrame(() => {
          if (cancelled) return;
          pendingMermaidRenderRef.current = false;
          setFlowUpdateState(null);
        });
      }
    })();
    return () => {
      cancelled = true;
      if (finishFrame !== null) {
        window.cancelAnimationFrame(finishFrame);
      }
    };
  }, [mermaidDiagram, mermaidRenderVersion, activeTab]);

  const handleConfigUpdate = useCallback(async () => {
    try {
      const updated = await api.agents.get(id);
      setAgent(updated);
      await Promise.allSettled([loadVersions(), loadCredentials()]);
    } catch {}
  }, [id, loadCredentials, loadVersions]);

  const handleBuildStart = useCallback(() => {
    pendingMermaidRenderRef.current = false;
    setFlowUpdateState({
      mode: mermaidDiagram ? "update" : "create",
    });
  }, [mermaidDiagram]);

  const handleBuildFinish = useCallback(() => {
    if (!pendingMermaidRenderRef.current) {
      setFlowUpdateState(null);
    }
  }, []);

  useEffect(() => {
    if (activeTab !== "credentials") return;
    loadCredentials();
  }, [activeTab, loadCredentials]);

  useEffect(() => {
    const handleMessage = (event: MessageEvent) => {
      if (event.data?.type !== "oauth_complete") return;
      const provider = typeof event.data?.integration === "string" ? event.data.integration : null;
      loadCredentials();
      if (provider) {
        setRecentlySavedSkill(provider);
        setTimeout(() => setRecentlySavedSkill(null), 3000);
      }
    };
    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, [loadCredentials]);

  // ── Credential Modal ──────────────────────────────────────────

  const openCredentialModal = async (card: ActionCard) => {
    // Unified: always open the shared IntegrationConnectionDialog
    try {
      const [integrations, apps] = await Promise.all([
        api.integrations.list(),
        api.oauthApps.list(),
      ]);
      const integration = integrations.find((item) => item.name === card.skill) ?? null;
      if (!integration) return;

      const existing = credentials.find((c) => c.provider === card.skill) || null;
      setOAuthApps(apps);
      setSelectedIntegration(integration);
      setSelectedExisting(existing);
      setOverrideDialogOpen(true);
    } catch {
      toast.error("Failed to load integration details. Please try again.");
    }
  };

  const refreshOAuthApps = useCallback(async (): Promise<OAuthApp[] | null> => {
    try {
      const apps = await api.oauthApps.list();
      setOAuthApps(apps);
      return apps;
    } catch {
      return null;
    }
  }, []);

  const openCredentialForProvider = async (provider: string) => {
    try {
      const [integrations, apps] = await Promise.all([
        api.integrations.list(),
        api.oauthApps.list(),
      ]);
      const integration = integrations.find((item) => item.name === provider) ?? null;
      if (!integration) {
        router.push("/dashboard/integrations");
        return;
      }
      setOAuthApps(apps);
      setSelectedIntegration(integration);
      setSelectedExisting(null);
      setOverrideDialogOpen(true);
    } catch {
      router.push("/dashboard/integrations");
    }
  };

  const editCredential = async (cred: Credential) => {
    try {
      const [integrations, apps] = await Promise.all([
        api.integrations.list(),
        api.oauthApps.list(),
      ]);
      const integration = integrations.find((item) => item.name === cred.provider) ?? null;
      if (!integration) return;

      setOAuthApps(apps);
      setSelectedIntegration(integration);
      setSelectedExisting(cred);
      setOverrideDialogOpen(true);
    } catch {
      // ignore
    }
  };

  const deleteCredential = async (credId: string) => {
    try {
      await api.credentials.delete(id, credId);
      await loadCredentials();
    } catch {
      /* ignore */
    }
  };

  const openNewCredential = useCallback(() => {
    router.push("/dashboard/integrations");
  }, [router]);

  const existingOAuthApp = selectedIntegration
    ? (oauthApps.find((a) => a.integration_name === selectedIntegration.name) ?? null)
    : null;

  // ── Deploy ────────────────────────────────────────────────────

  const openDeployDialog = useCallback(() => {
    if (versionOptions.length === 0) return;
    const targetVersion = versionOptions[0]?.version ?? null;
    setDeployDialogOpen(true);
    setSelectedDeployVersion(targetVersion);
    setDeployPage(0);
    setSelectedVersionConfig(versionOptions[0]?.config ?? null);
  }, [versionOptions]);

  const selectDeployVersion = useCallback((version: AgentVersion) => {
    setSelectedDeployVersion(version.version);
    setSelectedVersionConfig(version.config);
  }, []);

  const handleDeploy = useCallback(async () => {
    if (selectedDeployVersion === null) return;
    setDeployLoading(true);
    try {
      const updated = await api.agents.deploy(id, selectedDeployVersion);
      setAgent(updated);
      setDeployDialogOpen(false);
      await Promise.allSettled([loadVersions(), loadCredentials()]);
    } catch {
      /* ignore */
    } finally {
      setDeployLoading(false);
    }
  }, [id, loadCredentials, loadVersions, selectedDeployVersion]);

  const handleRevert = useCallback(async () => {
    if (selectedDeployVersion === null) return;
    setRevertLoading(true);
    try {
      const created = await api.agents.revert(id, selectedDeployVersion);
      setDeployDialogOpen(false);
      await Promise.allSettled([loadVersions(), loadCredentials()]);
      setDeployPage(0);
      setSelectedDeployVersion(created.version);
      setSelectedVersionConfig(created.config);
    } catch {
      // ignore
    } finally {
      setRevertLoading(false);
    }
  }, [id, loadCredentials, loadVersions, selectedDeployVersion]);

  const activeVersionNumber = agent?.status === "active" ? agent.active_version : null;
  const latestVersion = versionOptions[0]?.version ?? null;
  const displayedVersionNumber = latestVersion;
  const latestIsActive = Boolean(
    latestVersion !== null && activeVersionNumber !== null && activeVersionNumber === latestVersion
  );
  const displayedIsActive = Boolean(
    displayedVersionNumber !== null &&
    activeVersionNumber !== null &&
    displayedVersionNumber === activeVersionNumber
  );
  const showVersionStateBadges = Boolean(
    agent && !versionsLoading && displayedVersionNumber !== null
  );
  const currentVersionLabel = displayedVersionNumber ?? "-";
  const activeVersionLabel = activeVersionNumber ?? "-";
  const totalDeployPages = Math.max(1, Math.ceil(versionOptions.length / pageSize));
  const selectedIsLatest =
    selectedDeployVersion !== null && selectedDeployVersion === latestVersion;
  const pagedVersions = versionOptions.slice(
    deployPage * pageSize,
    deployPage * pageSize + pageSize
  );
  const shouldShowPagination = totalDeployPages > 1;
  const hasVersions = versionOptions.length > 0;

  const openRenameDialog = useCallback(() => {
    if (!agent) return;
    setRenameValue(agent.name);
    setRenameDialogOpen(true);
  }, [agent]);

  const submitRename = useCallback(async () => {
    if (!agent) return;
    const nextName = renameValue.trim();
    if (!nextName || nextName === agent.name) {
      setRenameDialogOpen(false);
      return;
    }
    setRenameLoading(true);
    try {
      const updated = await api.agents.update(id, { name: nextName });
      setAgent(updated);
      setRenameDialogOpen(false);
    } catch {
      // ignore
    } finally {
      setRenameLoading(false);
    }
  }, [agent, id, renameValue]);

  const submitDelete = useCallback(async () => {
    if (!agent || deleteConfirmName !== agent.name) return;
    setDeleteLoading(true);
    try {
      await api.agents.delete(id);
      router.push("/dashboard/agents");
    } catch {
      // ignore
    } finally {
      setDeleteLoading(false);
    }
  }, [agent, deleteConfirmName, id, router]);

  const copyAgentId = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(id);
      toast.success("Agent ID copied");
    } catch {
      toast.error("Failed to copy Agent ID");
    }
  }, [id]);

  const exportFullConfig = useCallback(async () => {
    try {
      const payload = await api.agents.export(id);
      const blob = new Blob([JSON.stringify(payload, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      const slug = (agent?.name || "agent")
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/(^-|-$)/g, "");
      a.href = url;
      a.download = `${slug || "agent"}-full-config.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      toast.success("Full config exported");
    } catch {
      toast.error("Failed to export full config");
    }
  }, [agent?.name, id]);

  // ── Render ────────────────────────────────────────────────────

  return (
    <div className="fixed inset-x-0 bottom-0 top-0 left-60 flex bg-background text-foreground z-30">
      {/* ═══ LEFT PANEL — Chat ═══ */}
      <div className="flex w-120 shrink-0 flex-col min-w-0 border-r border-border">
        {/* Header - Command Center Style */}
        <div className="flex h-14 items-center justify-between border-b border-border/60 bg-accent/10 px-4 shrink-0 backdrop-blur-md">
          <div className="flex items-center gap-4">
            <Link href="/dashboard/agents">
              <Button
                variant="ghost"
                size="icon"
                className="size-8 rounded-xl hover:bg-background/80 hover:shadow-sm transition-all active:scale-95"
              >
                <HugeiconsIcon icon={ArrowLeft01Icon} className="size-4.5" />
              </Button>
            </Link>

            <div className="flex flex-col">
              <div className="flex items-center gap-2.5">
                <div className="size-7 rounded-lg bg-primary/10 flex items-center justify-center text-primary group-hover:scale-110 transition-transform">
                  <HugeiconsIcon icon={Robot01Icon} className="size-4" />
                </div>
                <h2 className="text-sm font-bold tracking-tight text-foreground truncate max-w-[180px]">
                  {agent?.name || "Loading..."}
                </h2>

                {showVersionStateBadges && (
                  <div className="flex items-center gap-1.5 ml-1">
                    {displayedIsActive ? (
                      <span className="flex h-5 items-center px-2 rounded-full border border-emerald-500/20 bg-emerald-500/10 text-[10px] font-black uppercase tracking-widest text-emerald-600">
                        active v{currentVersionLabel}
                      </span>
                    ) : (
                      <>
                        <span className="flex h-5 items-center px-1.5 rounded-full border border-border/60 bg-accent/40 text-[10px] font-black uppercase tracking-widest text-muted-foreground">
                          v{currentVersionLabel}
                        </span>
                        {activeVersionNumber !== null && (
                          <span className="flex h-5 items-center px-2 rounded-full border border-emerald-500/20 bg-emerald-500/10 text-[10px] font-black uppercase tracking-widest text-emerald-600">
                            active v{activeVersionLabel}
                          </span>
                        )}
                      </>
                    )}
                  </div>
                )}
              </div>
            </div>
          </div>

          <div className="flex items-center gap-1">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-8 rounded-xl hover:bg-background/80 hover:shadow-sm transition-all"
                >
                  <HugeiconsIcon icon={MoreHorizontalIcon} className="size-4.5" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-52 rounded-xl">
                <DropdownMenuItem
                  onClick={() =>
                    router.push(`/dashboard/calls?agent_ids=${encodeURIComponent(id)}`)
                  }
                  className="text-xs font-bold py-2"
                >
                  <HugeiconsIcon icon={Clock01Icon} className="size-4" />
                  Execution Logs
                </DropdownMenuItem>
                <DropdownMenuItem onClick={openRenameDialog} className="text-xs font-bold py-2">
                  <HugeiconsIcon icon={PencilEdit01Icon} className="size-4" />
                  Rename Entity
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={openDeployDialog}
                  className="text-xs font-bold py-2"
                  disabled={!hasVersions}
                >
                  <HugeiconsIcon icon={Rocket01Icon} className="size-4" />
                  Version Sync
                </DropdownMenuItem>
                <DropdownMenuItem onClick={copyAgentId} className="text-xs font-bold py-2">
                  <HugeiconsIcon icon={Copy01Icon} className="size-4" />
                  Copy Agent ID
                </DropdownMenuItem>
                <DropdownMenuItem onClick={exportFullConfig} className="text-xs font-bold py-2">
                  <HugeiconsIcon icon={Download01Icon} className="size-4" />
                  Download Agent Config
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  variant="destructive"
                  className="text-xs font-bold py-2"
                  onClick={() => {
                    setDeleteConfirmName("");
                    setDeleteDialogOpen(true);
                  }}
                >
                  <HugeiconsIcon icon={Delete02Icon} className="size-4" />
                  Purge Agent
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>

        <ChatPanel
          agentId={id}
          messages={messages}
          setMessages={setMessages}
          onBuildStart={handleBuildStart}
          onBuildFinish={handleBuildFinish}
          onConfigUpdate={handleConfigUpdate}
          onMermaidUpdate={(diagram) => {
            pendingMermaidRenderRef.current = true;
            setMermaidDiagram(diagram);
            setMermaidRenderVersion((current) => current + 1);
          }}
          credentials={credentials}
          recentlySavedSkill={recentlySavedSkill}
          onOpenCredentialModal={openCredentialModal}
          onDiff={(desc) => {
            setLastDiff(desc);
            setActiveTab("config");
          }}
        />
      </div>

      {/* ═══ RIGHT PANEL — Preview ═══ */}
      <PreviewPanel
        activeTab={activeTab}
        setActiveTab={setActiveTab}
        credentialCount={credentials.length}
        headerActions={
          agent?.current_config ? (
            <Button
              size="sm"
              className="rounded-full font-semibold gap-1.5 h-7 text-xs"
              onClick={openDeployDialog}
              disabled={!hasVersions}
            >
              {latestIsActive ? (
                <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-3" />
              ) : (
                <HugeiconsIcon icon={Rocket01Icon} className="size-3" />
              )}
              Versions & Deploy
            </Button>
          ) : null
        }
      >
        {/* ── Flow ── */}
        {activeTab === "flow" && (
          <div className="p-4 h-full flex flex-col">
            <div className="flex flex-col flex-1 min-h-0 space-y-3">
              <div className="flex items-center justify-between shrink-0">
                <h3 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
                  Agent Flow
                </h3>
              </div>
              <div className="relative flex-1 w-full min-h-[500px] flex flex-col items-center justify-center overflow-hidden rounded-xl border border-border/40 shadow-sm bg-accent/5 backdrop-blur-[2px]">
                {/* Dot grid + vignette only when a diagram is loaded */}
                {mermaidDiagram && (
                  <>
                    <div
                      className="absolute inset-0 pointer-events-none opacity-20"
                      style={{
                        backgroundImage:
                          "radial-gradient(circle at center, var(--foreground) 1px, transparent 1px)",
                        backgroundSize: "24px 24px",
                      }}
                    />
                    <div
                      className="absolute inset-0 pointer-events-none"
                      style={{
                        background:
                          "radial-gradient(ellipse at center, transparent 40%, var(--background) 100%)",
                      }}
                    />
                  </>
                )}
                {mermaidDiagram ? (
                  <div className="absolute inset-0 z-10 cursor-grab active:cursor-grabbing">
                    <TransformWrapper
                      initialScale={1}
                      minScale={0.1}
                      maxScale={4}
                      centerOnInit
                      limitToBounds={false}
                      wheel={{ step: 0.1 }}
                      pinch={{ step: 5 }}
                      panning={{ velocityDisabled: false }}
                    >
                      <TransformComponent
                        wrapperClass="!w-full !h-full"
                        contentClass="flex items-center justify-center"
                      >
                        <div
                          ref={mermaidRef}
                          className={`inline-flex items-center justify-center p-12 [&>svg]:bg-transparent! [&_svg]:max-w-none [&_svg]:h-auto transition-all duration-500 ${
                            flowUpdateState
                              ? "[&_svg]:opacity-20 [&_svg]:blur-xs"
                              : "[&_svg]:opacity-100"
                          }`}
                        />
                      </TransformComponent>
                    </TransformWrapper>
                  </div>
                ) : (
                  <div className="flex min-h-[420px] items-center justify-center p-6">
                    {!flowUpdateState && (
                      <EmptyState
                        icon={<HugeiconsIcon icon={GitBranchIcon} className="size-5 opacity-30" />}
                        title="No flow yet"
                        desc="Describe your agent to generate a flow diagram."
                      />
                    )}
                  </div>
                )}
                {flowUpdateState && <FlowBusyOverlay mode={flowUpdateState.mode} />}
              </div>
            </div>
          </div>
        )}

        {activeTab === "config" && (
          <div className="p-5">
            {agent?.current_config ? (
              <AgentConfigEditor
                agentId={id}
                agent={agent}
                onUpdate={setAgent}
                lastDiff={lastDiff}
              />
            ) : (
              <EmptyState
                icon={<HugeiconsIcon icon={CodeIcon} className="size-5 opacity-30" />}
                title="No config yet"
                desc="Configuration appears once your agent is defined."
              />
            )}
          </div>
        )}

        {/* ── Credentials ── */}
        {activeTab === "credentials" && (
          <div className="p-5 space-y-4">
            <div className="flex items-center justify-between">
              <h3 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
                Connected Integrations
              </h3>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs gap-1 rounded-lg"
                onClick={() => openNewCredential()}
              >
                <HugeiconsIcon icon={LeftToRightListBulletIcon} className="size-3" /> Browse
                Integrations
              </Button>
            </div>

            {credLoading ? (
              <div className="flex justify-center py-12">
                <Spinner className="size-5 text-muted-foreground" />
              </div>
            ) : credentials.length > 0 ? (
              <div className="space-y-2">
                {credentials.map((cred) => (
                  <div
                    key={cred.id}
                    className="flex items-center justify-between p-3 rounded-xl border border-border bg-background transition-colors hover:bg-accent/2"
                  >
                    <div className="flex items-center gap-2.5">
                      <div className="relative size-8 rounded-lg bg-secondary flex items-center justify-center shrink-0">
                        <IntegrationIcon
                          name={cred.provider}
                          iconHint="shield"
                          size="size-4"
                          brandSize="size-7"
                        />
                      </div>
                      <div>
                        <span className="text-xs font-medium">{cred.name}</span>
                        <div className="flex items-center gap-1.5 mt-0.5">
                          <Badge variant="secondary" className="text-[10px] h-4 px-1.5">
                            {cred.provider}
                          </Badge>
                          {cred.is_default ? (
                            <span className="text-[10px] text-muted-foreground italic">
                              platform default
                            </span>
                          ) : (
                            <span className="text-[10px] text-muted-foreground">
                              {cred.auth_type}
                            </span>
                          )}
                        </div>
                      </div>
                    </div>
                    <div className="flex gap-1">
                      {cred.is_default ? (
                        // Default: only allow Override (creates a per-agent credential)
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-6 text-[10px] px-2 rounded-md gap-1 text-foreground"
                          onClick={() => openCredentialForProvider(cred.provider)}
                        >
                          <HugeiconsIcon icon={Settings03Icon} className="size-2.5" />
                          Override
                        </Button>
                      ) : (
                        // Per-agent: full edit / delete
                        <>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-7 rounded-md"
                            onClick={() => editCredential(cred)}
                          >
                            <HugeiconsIcon
                              icon={PencilEdit01Icon}
                              className="size-3 text-muted-foreground"
                            />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-7 rounded-md"
                            onClick={() => deleteCredential(cred.id)}
                          >
                            <HugeiconsIcon
                              icon={Delete02Icon}
                              className="size-3 text-destructive"
                            />
                          </Button>
                        </>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            ) : (
              importedConnections.length > 0 ? (
                <div className="space-y-2">
                  {importedConnections.map((conn, idx) => (
                    <div
                      key={`${conn.provider}-${idx}`}
                      className="flex items-center justify-between p-3 rounded-xl border border-border bg-background"
                    >
                      <div className="flex items-center gap-2.5">
                        <div className="relative size-8 rounded-lg bg-secondary flex items-center justify-center shrink-0">
                          <IntegrationIcon
                            name={conn.provider}
                            iconHint="shield"
                            size="size-4"
                            brandSize="size-7"
                          />
                        </div>
                        <div>
                          <span className="text-xs font-medium">
                            {conn.name || conn.provider}
                          </span>
                          <div className="flex items-center gap-1.5 mt-0.5">
                            <Badge variant="secondary" className="text-[10px] h-4 px-1.5">
                              {conn.provider}
                            </Badge>
                            <span className="text-[10px] text-muted-foreground">
                              {conn.status}
                            </span>
                          </div>
                        </div>
                      </div>
                      <Button
                        variant="outline"
                        size="sm"
                        className="h-6 text-[10px] px-2 rounded-md gap-1 text-foreground"
                        onClick={() => openCredentialForProvider(conn.provider)}
                      >
                        <HugeiconsIcon icon={Settings03Icon} className="size-2.5" />
                        Connect
                      </Button>
                    </div>
                  ))}
                </div>
              ) : (
                <EmptyState icon={<HugeiconsIcon icon={Key02Icon} className="size-5 opacity-30" />} title="No credentials" desc="Connect integrations from the chat or configure defaults in Integrations." />
              )
            )}
          </div>
        )}

        {/* ── Test ── */}
        {activeTab === "test" && (
          <TestPanel agentId={id} agent={agent} onGoToConfig={() => setActiveTab("config")} />
        )}
      </PreviewPanel>

      <Dialog
        open={deployDialogOpen}
        onOpenChange={(open) => {
          setDeployDialogOpen(open);
          if (!open) {
            setDeployLoading(false);
            setRevertLoading(false);
          }
        }}
      >
        <DialogContent className="sm:max-w-[760px] rounded-2xl border-border">
          <DialogHeader>
            <DialogTitle className="text-base font-semibold">Versions and Deploy</DialogTitle>
            <DialogDescription className="text-sm text-muted-foreground leading-relaxed">
              Deploy sets a selected version as Active, so calls and tests run with that config.
              Revert creates a new latest version by copying a past version, so you can safely roll
              back behavior without changing the current Active version until you deploy again.
            </DialogDescription>
          </DialogHeader>

          {versionsLoading ? (
            <div className="flex justify-center py-10">
              <Spinner className="size-5 text-muted-foreground" />
            </div>
          ) : versionOptions.length === 0 ? (
            <div className="py-8">
              <EmptyState
                icon={<HugeiconsIcon icon={GitBranchIcon} className="size-5 opacity-30" />}
                title="No versions available"
                desc="Create a config version in Builder first."
              />
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-4 lg:grid-cols-5">
              <div className="space-y-3 lg:col-span-2">
                <div className="max-h-[420px] overflow-y-auto space-y-2 pr-1">
                  {pagedVersions.map((version) => {
                    const isSelected = selectedDeployVersion === version.version;
                    const isActive = activeVersionNumber === version.version;
                    const updatedText = new Date(version.created_at).toLocaleString();
                    return (
                      <button
                        key={version.id}
                        type="button"
                        onClick={() => selectDeployVersion(version)}
                        className={`w-full rounded-xl border px-3 py-2.5 text-left transition-colors ${
                          isSelected
                            ? "border-primary bg-primary/5"
                            : "border-border bg-background hover:bg-accent/30"
                        }`}
                      >
                        <div className="flex items-center justify-between gap-3">
                          <div className="flex items-center gap-3 min-w-0">
                            <span className="text-xs font-semibold text-foreground shrink-0">
                              v{version.version}
                            </span>
                            {isActive ? (
                              <Badge className="h-5 px-1.5 text-[10px] uppercase tracking-wider shrink-0">
                                Active
                              </Badge>
                            ) : null}
                          </div>
                          <span className="flex items-center gap-1.5 text-[10px] text-muted-foreground shrink-0">
                            <HugeiconsIcon icon={Clock01Icon} className="size-3 shrink-0" />
                            <span>{updatedText}</span>
                          </span>
                        </div>
                      </button>
                    );
                  })}
                </div>
                {shouldShowPagination ? (
                  <div className="flex items-center justify-end gap-2">
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="h-7 px-3 text-xs"
                      disabled={deployPage === 0}
                      onClick={() => setDeployPage((p) => Math.max(0, p - 1))}
                    >
                      上一页
                    </Button>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="h-7 px-3 text-xs"
                      disabled={deployPage >= totalDeployPages - 1}
                      onClick={() => setDeployPage((p) => Math.min(totalDeployPages - 1, p + 1))}
                    >
                      下一页
                    </Button>
                  </div>
                ) : null}
              </div>
              {selectedDeployVersion === null ? (
                <div className="flex h-[420px] items-center justify-center lg:col-span-3">
                  <p className="text-xs text-muted-foreground">
                    Select a version to preview its config.
                  </p>
                </div>
              ) : selectedVersionConfig ? (
                <pre className="h-[420px] overflow-auto rounded-lg border border-border bg-accent/20 p-3 font-mono text-[10px] leading-relaxed text-muted-foreground whitespace-pre-wrap wrap-break-word lg:col-span-3">
                  {JSON.stringify(selectedVersionConfig, null, 2)}
                </pre>
              ) : (
                <div className="flex h-[420px] items-center justify-center lg:col-span-3">
                  <Spinner className="size-4 text-muted-foreground" />
                </div>
              )}
            </div>
          )}

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setDeployDialogOpen(false)}>
              Cancel
            </Button>
            {!selectedIsLatest && selectedDeployVersion !== null ? (
              <Button
                type="button"
                variant="outline"
                disabled={revertLoading || deployLoading}
                onClick={() => void handleRevert()}
              >
                {revertLoading ? "Reverting..." : `Revert to V${selectedDeployVersion}`}
              </Button>
            ) : null}
            <Button
              type="button"
              disabled={
                deployLoading ||
                revertLoading ||
                selectedDeployVersion === null ||
                selectedDeployVersion === activeVersionNumber
              }
              onClick={() => void handleDeploy()}
            >
              {deployLoading ? "Deploying..." : `Deploy v${selectedDeployVersion ?? ""}`}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <IntegrationConnectionDialog
        open={overrideDialogOpen}
        onOpenChange={setOverrideDialogOpen}
        mode="override"
        agentId={id}
        integration={selectedIntegration}
        existing={selectedExisting}
        oauthReady={
          selectedIntegration
            ? oauthApps.some((a) => a.integration_name === selectedIntegration.name)
            : false
        }
        onSaved={() => loadCredentials()}
        onOAuthStarted={() => {
          // popup is opened; on oauth_complete message we refresh credentials
        }}
        onConfigureOAuthApp={() => {
          setOauthEditReturnTarget("override");
          setRegisterOAuthOpen(true);
        }}
      />

      <OAuthAppRegistrationDialog
        open={registerOAuthOpen}
        onOpenChange={(open) => {
          setRegisterOAuthOpen(open);
          if (!open) setOauthEditReturnTarget("none");
        }}
        integration={selectedIntegration}
        existing={existingOAuthApp}
        onSaved={async () => {
          const apps = await refreshOAuthApps();
          if (!apps) {
            toast.error("Failed to refresh OAuth apps.");
            return;
          }
          if (oauthEditReturnTarget === "override") {
            setRegisterOAuthOpen(false);
            setOauthEditReturnTarget("none");
            setOverrideDialogOpen(true);
          }
        }}
      />

      <Dialog
        open={renameDialogOpen}
        onOpenChange={(open) => {
          setRenameDialogOpen(open);
          if (!open) setRenameLoading(false);
        }}
      >
        <DialogContent className="sm:max-w-[420px] rounded-2xl border-border">
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void submitRename();
            }}
            className="space-y-4"
          >
            <DialogHeader>
              <DialogTitle className="text-sm font-semibold">Rename Agent</DialogTitle>
              <DialogDescription className="text-xs text-muted-foreground">
                Update the display name of this Agent.
              </DialogDescription>
            </DialogHeader>
            <Input
              ref={renameInputRef}
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              className="h-10 rounded-xl text-sm"
            />
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setRenameDialogOpen(false)}>
                Cancel
              </Button>
              <Button type="submit" disabled={renameLoading || !renameValue.trim()}>
                {renameLoading ? "Renaming..." : "Rename"}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <Dialog
        open={deleteDialogOpen}
        onOpenChange={(open) => {
          setDeleteDialogOpen(open);
          if (!open) {
            setDeleteConfirmName("");
            setDeleteLoading(false);
          }
        }}
      >
        <DialogContent className="sm:max-w-[460px] rounded-2xl border-border">
          <DialogHeader>
            <DialogTitle className="text-sm font-semibold">Delete Agent?</DialogTitle>
            <DialogDescription className="text-xs text-muted-foreground">
              This action permanently deletes this Agent and related data including call logs. To
              confirm, type the Agent name exactly:
              <span className="block mt-3 font-mono text-foreground">{agent?.name || "-"}</span>
            </DialogDescription>
          </DialogHeader>
          <Input
            value={deleteConfirmName}
            onChange={(e) => setDeleteConfirmName(e.target.value)}
            placeholder="Type agent name to confirm"
            className="h-10 rounded-xl text-sm"
          />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setDeleteDialogOpen(false)}>
              Cancel
            </Button>
            <Button
              type="button"
              variant="destructive"
              disabled={!agent || deleteConfirmName !== agent.name || deleteLoading}
              onClick={() => void submitDelete()}
            >
              {deleteLoading ? "Deleting..." : "Delete Agent"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// ── Shared Components ───────────────────────────────────────────

function EmptyState({ icon, title, desc }: { icon: React.ReactNode; title: string; desc: string }) {
  return (
    <div className="flex flex-col items-center justify-center py-24 text-center px-8 transition-all animate-in fade-in zoom-in-95 duration-700">
      <div className="relative mb-6">
        <div className="absolute inset-0 bg-primary/20 blur-2xl rounded-full scale-150 animate-pulse" />
        <div className="relative size-16 rounded-2xl bg-linear-to-tr from-accent to-accent/50 border border-border/60 flex items-center justify-center text-muted-foreground shadow-inner">
          <div className="opacity-60 scale-125">{icon}</div>
        </div>
      </div>
      <h3 className="text-base font-bold tracking-tight text-foreground mb-2">{title}</h3>
      <p className="text-xs text-muted-foreground/80 max-w-[280px] leading-relaxed font-medium">
        {desc}
      </p>
    </div>
  );
}

function FlowBusyOverlay({ mode }: { mode: "create" | "update" }) {
  const isUpdate = mode === "update";

  return (
    <div className="absolute inset-0 flex items-center justify-center bg-background/88 px-6 backdrop-blur-[2px]">
      <div className="w-full max-w-sm rounded-3xl border border-border/80 bg-card/95 p-6 text-center shadow-[0_24px_80px_rgba(0,0,0,0.12)]">
        <div className="mx-auto flex size-12 items-center justify-center rounded-2xl bg-primary/10 text-primary">
          <HugeiconsIcon icon={GitBranchIcon} className="size-5" />
        </div>
        <div className="mt-4 inline-flex items-center gap-2 rounded-full border border-primary/15 bg-primary/5 px-3 py-1 text-[10px] font-semibold uppercase tracking-[0.22em] text-primary">
          <Spinner className="size-3" />
          {isUpdate ? "Updating" : "Generating"}
        </div>
        <h3 className="mt-4 text-base font-semibold text-foreground">
          {isUpdate ? "Updating flow" : "Generating flow"}
        </h3>
        <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
          {isUpdate
            ? "Updating the call flow to match your latest changes."
            : "Generating a call flow from your current setup."}
        </p>
        <div className="mt-6 space-y-2.5">
          <Skeleton className="mx-auto h-3 w-[82%] rounded-full bg-primary/10" />
          <Skeleton className="mx-auto h-3 w-[68%] rounded-full bg-primary/10" />
          <Skeleton className="mx-auto h-3 w-[74%] rounded-full bg-primary/10" />
        </div>
      </div>
    </div>
  );
}

export default function AgentDetailPage(props: { params: Promise<{ id: string }> }) {
  return (
    <Suspense
      fallback={
        <div className="flex h-screen items-center justify-center p-4">Loading agent...</div>
      }
    >
      <AgentDetailPageContent {...props} />
    </Suspense>
  );
}
