"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  Alert01Icon,
  ShieldUserIcon,
  ConnectIcon,
  Settings01Icon,
  Unlink01Icon,
  LockKeyIcon,
} from "@hugeicons/core-free-icons";
import { useState, useEffect, useCallback, useMemo } from "react";
import {
  api,
  type OAuthApp,
  type IntegrationSummary,
  type DefaultConnection,
} from "@/lib/api/client";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { IntegrationIcon } from "@/components/ui/integration-icon";
import { PageHeader } from "@/components/ui/page-header";
import { Spinner } from "@/components/ui/spinner";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import {
  IntegrationConnectionDialog,
  OAuthAppRegistrationDialog,
} from "@/components/integrations/connection-dialogs";

function getCategoryColor(category: string) {
  switch (category) {
    case "messaging":
      return "bg-blue-500/10 text-blue-600 dark:text-blue-400";
    case "scheduling":
    case "productivity":
      return "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400";
    case "database":
      return "bg-violet-500/10 text-violet-600 dark:text-violet-400";
    case "crm":
    case "marketing":
      return "bg-orange-500/10 text-orange-600 dark:text-orange-400";
    case "ai":
    case "dev-tools":
      return "bg-sky-500/10 text-sky-600 dark:text-sky-400";
    case "communication":
      return "bg-pink-500/10 text-pink-600 dark:text-pink-400";
    case "payment":
    case "e-commerce":
      return "bg-green-500/10 text-green-600 dark:text-green-400";
    default:
      return "bg-muted text-muted-foreground";
  }
}

function prettyAuthType(authType: string): string {
  return authType === "oauth2" ? "OAuth" : "BYOK";
}
interface IntegrationCardProps {
  integration: IntegrationSummary;
  connection: DefaultConnection | undefined;
  supportsByok: boolean;
  oauthReady: boolean;
  onConnect: () => void;
  onDisconnect: () => void;
  onConfigureOAuth: () => void;
}

function IntegrationCard({
  integration,
  connection,
  supportsByok,
  oauthReady,
  onConnect,
  onDisconnect,
  onConfigureOAuth,
}: IntegrationCardProps) {
  const supportsOAuth = integration.auth_type === "oauth2";
  const isConnected = !!connection;

  return (
    <div
      className={`flat-card border-none ring-1 p-5 flex flex-col gap-4 hover:shadow-md transition-shadow ${
        isConnected ? "ring-primary/30 shadow-sm" : "bg-muted/50 ring-foreground/10"
      }`}
    >
      <div className="flex items-center gap-3">
        <div
          className={`relative size-10 rounded-xl flex items-center justify-center shrink-0 ${isConnected ? "bg-primary/10" : "bg-secondary"}`}
        >
          <IntegrationIcon
            name={integration.name}
            iconHint={integration.icon}
            size="size-5"
            className={isConnected ? "text-primary" : "text-muted-foreground"}
          />
        </div>

        <div className="flex-1 min-w-0">
          <div className="flex items-center justify-between gap-2">
            <h3 className="text-sm font-semibold text-foreground truncate">
              {integration.display_name}
            </h3>
            <div className="flex items-center gap-1.5 shrink-0">
              {supportsOAuth && (
                <TooltipProvider>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <button
                        type="button"
                        onClick={onConfigureOAuth}
                        className="group inline-flex items-center"
                        id={`oauth-badge-${integration.name}`}
                      >
                        <Badge className="h-6 inline-flex items-center bg-background text-muted-foreground border border-border/60 shadow-none text-[10px] font-medium rounded-full px-2.5 cursor-pointer hover:bg-muted/40 leading-none">
                          {!oauthReady ? (
                            <HugeiconsIcon
                              icon={Alert01Icon}
                              className="size-2.5 text-amber-500 mr-0.5"
                              aria-hidden
                            />
                          ) : (
                            <span className="inline-flex items-center overflow-hidden transition-all group-hover:mr-0.5 group-hover:w-3.5 w-0">
                              <HugeiconsIcon
                                icon={Settings01Icon}
                                className="size-3 text-emerald-600 opacity-0 transition-opacity group-hover:opacity-100"
                              />
                            </span>
                          )}
                          OAuth
                        </Badge>
                      </button>
                    </TooltipTrigger>
                    <TooltipContent side="top" sideOffset={6}>
                      {!oauthReady ? "Configure OAuth App" : "Update OAuth App"}
                    </TooltipContent>
                  </Tooltip>
                </TooltipProvider>
              )}

              {supportsByok && (
                <Badge className="h-6 inline-flex items-center bg-background text-muted-foreground border border-border/60 shadow-none text-[10px] font-medium rounded-full px-2.5 leading-none">
                  BYOK
                </Badge>
              )}
            </div>
          </div>
        </div>
      </div>

      <p className="text-xs text-muted-foreground leading-relaxed line-clamp-2">
        {integration.description}
      </p>
      <div className="flex flex-wrap gap-1">
        {integration.categories.map((cat) => (
          <span
            key={cat}
            className={`text-[10px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-full ${getCategoryColor(cat)}`}
          >
            {cat}
          </span>
        ))}
      </div>

      {isConnected ? (
        <div className="py-3 px-4 rounded-lg bg-muted/50 flex items-center justify-between gap-3">
          <p className="text-[10px] font-medium text-foreground uppercase tracking-wider flex items-center gap-2 min-w-0">
            {connection.auth_type === "oauth2" ? (
              <HugeiconsIcon icon={ShieldUserIcon} className="size-4 shrink-0" />
            ) : (
              <HugeiconsIcon icon={LockKeyIcon} className="size-4 shrink-0" />
            )}
            <span className="truncate">Connected / {prettyAuthType(connection.auth_type)}</span>
          </p>
          <Button
            id={`disconnect-${integration.name}`}
            variant="ghost"
            size="sm"
            onClick={onDisconnect}
            aria-label={`Disconnect ${integration.display_name}`}
            className="h-6 w-6 p-0 text-destructive hover:text-destructive hover:bg-destructive/10"
          >
            <HugeiconsIcon icon={Unlink01Icon} className="size-3.5" />
          </Button>
        </div>
      ) : null}

      {!isConnected && (
        <div className="flex items-center gap-2 mt-auto pt-1">
          <Button
            id={`connect-${integration.name}`}
            variant="outline"
            size="sm"
            onClick={onConnect}
            className="flex-1 text-xs h-8 gap-1.5"
          >
            <HugeiconsIcon icon={Add01Icon} className="size-3.5" />
            Connect
          </Button>
        </div>
      )}
    </div>
  );
}

export default function IntegrationsPage() {
  const [integrations, setIntegrations] = useState<IntegrationSummary[]>([]);
  const [oauthApps, setOAuthApps] = useState<OAuthApp[]>([]);
  const [defaultConns, setDefaultConns] = useState<DefaultConnection[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");

  const [byokSupport, setByokSupport] = useState<Record<string, boolean>>({});

  const [registerOpen, setRegisterOpen] = useState(false);
  const [connectOpen, setConnectOpen] = useState(false);
  const [selectedIntegration, setSelectedIntegration] = useState<IntegrationSummary | null>(null);
  const [deleteDefaultTarget, setDeleteDefaultTarget] = useState<string | null>(null);
  const [deletingDefault, setDeletingDefault] = useState(false);
  const [oauthEditReturnTarget, setOauthEditReturnTarget] = useState<"none" | "connect">("none");

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [intList, appList, connList] = await Promise.all([
        api.integrations.list(),
        api.oauthApps.list(),
        api.integrations.listDefaultConnections(),
      ]);
      setIntegrations(intList);
      setOAuthApps(appList);
      setDefaultConns(connList);
    } catch {
      toast.error("Failed to load integrations");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (!integrations.length) return;
    const next: Record<string, boolean> = {};
    for (const integration of integrations) {
      next[integration.name] = integration.supports_byok;
    }
    setByokSupport(next);
  }, [integrations]);

  const refreshOAuthApps = useCallback(async (): Promise<OAuthApp[] | null> => {
    try {
      const apps = await api.oauthApps.list();
      setOAuthApps(apps);
      return apps;
    } catch {
      return null;
    }
  }, []);

  const refreshDefaultConnections = useCallback(async () => {
    try {
      const conns = await api.integrations.listDefaultConnections();
      setDefaultConns(conns);
    } catch {
      // ignore
    }
  }, []);

  const handleDeleteDefault = async () => {
    if (!deleteDefaultTarget) return;
    setDeletingDefault(true);
    try {
      await api.integrations.deleteDefaultConnection(deleteDefaultTarget);
      setDefaultConns((prev) => prev.filter((c) => c.provider !== deleteDefaultTarget));
      toast.success("Connection disconnected");
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Failed to disconnect");
    } finally {
      setDeletingDefault(false);
      setDeleteDefaultTarget(null);
    }
  };

  useEffect(() => {
    const handleMessage = (e: MessageEvent) => {
      if (e.data?.type === "oauth_complete") {
        api.integrations
          .listDefaultConnections()
          .then(setDefaultConns)
          .catch(() => {});
      }
    };
    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, []);

  const existingOAuthApp = selectedIntegration
    ? (oauthApps.find((a) => a.integration_name === selectedIntegration.name) ?? null)
    : null;

  const existingDefault = selectedIntegration
    ? (defaultConns.find((c) => c.provider === selectedIntegration.name) ?? null)
    : null;

  const filtered = integrations.filter(
    (i) =>
      !search ||
      i.display_name.toLowerCase().includes(search.toLowerCase()) ||
      i.description.toLowerCase().includes(search.toLowerCase()) ||
      i.categories.some((c) => c.toLowerCase().includes(search.toLowerCase()))
  );

  const sorted = useMemo(() => {
    return [...filtered].sort((a, b) => {
      const aConnected = defaultConns.some((c) => c.provider === a.name);
      const bConnected = defaultConns.some((c) => c.provider === b.name);
      if (aConnected !== bConnected) return aConnected ? -1 : 1;
      return a.display_name.localeCompare(b.display_name);
    });
  }, [filtered, defaultConns]);

  return (
    <div className="space-y-8" id="integrations-page">
      <PageHeader
        icon={ConnectIcon}
        title="Integrations"
        description="Connect apps with OAuth or BYOK for platform-wide default connections"
      />

      <div className="flex justify-start">
        <div className="relative">
          <input
            id="integrations-search"
            type="text"
            placeholder="Search integrations…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-64 h-9 rounded-lg bg-secondary/60 border border-border pl-9 pr-3 text-sm focus:outline-none focus:ring-2 focus:ring-ring placeholder:text-muted-foreground transition-all"
          />
          <HugeiconsIcon
            icon={ConnectIcon}
            className="absolute left-3 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground"
          />
        </div>
      </div>

      {loading ? (
        <div className="flex items-center justify-center py-24">
          <Spinner className="size-6 text-muted-foreground" />
        </div>
      ) : sorted.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-24 gap-3 text-center">
          <HugeiconsIcon icon={ConnectIcon} className="size-10 text-muted-foreground/30" />
          <p className="text-sm text-muted-foreground">
            {search ? "No integrations match your search" : "No integrations available"}
          </p>
        </div>
      ) : (
        <div
          className="grid gap-4"
          style={{
            gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))",
          }}
        >
          {sorted.map((integration) => {
            const connection = defaultConns.find((c) => c.provider === integration.name);
            const oauthReady = oauthApps.some((a) => a.integration_name === integration.name);
            const supportsByok = byokSupport[integration.name] ?? false;

            return (
              <IntegrationCard
                key={integration.name}
                integration={integration}
                connection={connection}
                supportsByok={supportsByok}
                oauthReady={oauthReady}
                onConnect={() => {
                  setSelectedIntegration(integration);
                  setConnectOpen(true);
                }}
                onDisconnect={() => setDeleteDefaultTarget(integration.name)}
                onConfigureOAuth={() => {
                  setSelectedIntegration(integration);
                  setOauthEditReturnTarget("none");
                  setRegisterOpen(true);
                }}
              />
            );
          })}
        </div>
      )}

      <OAuthAppRegistrationDialog
        open={registerOpen}
        onOpenChange={(open) => {
          setRegisterOpen(open);
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
          if (oauthEditReturnTarget === "connect") {
            setRegisterOpen(false);
            setOauthEditReturnTarget("none");
            setConnectOpen(true);
          }
        }}
      />

      <IntegrationConnectionDialog
        open={connectOpen}
        onOpenChange={setConnectOpen}
        mode="default"
        integration={selectedIntegration}
        existing={existingDefault}
        oauthReady={
          selectedIntegration
            ? oauthApps.some((a) => a.integration_name === selectedIntegration.name)
            : false
        }
        onSaved={() => void refreshDefaultConnections()}
        onOAuthStarted={() => {
          // popup is opened; postMessage listener refreshes connection list on completion
        }}
        onConfigureOAuthApp={() => {
          setOauthEditReturnTarget("connect");
          setRegisterOpen(true);
        }}
      />

      <AlertDialog
        open={!!deleteDefaultTarget}
        onOpenChange={(open) => {
          if (!open) setDeleteDefaultTarget(null);
        }}
      >
        <AlertDialogContent className="rounded-2xl" id="delete-default-conn-dialog">
          <AlertDialogHeader>
            <AlertDialogTitle className="text-sm font-semibold">Disconnect app?</AlertDialogTitle>
            <AlertDialogDescription className="text-sm text-muted-foreground">
              Disconnecting{" "}
              <span className="font-medium text-foreground">
                {integrations.find((i) => i.name === deleteDefaultTarget)?.display_name ??
                  deleteDefaultTarget}
              </span>{" "}
              removes its platform default connection.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter className="gap-2">
            <AlertDialogCancel className="text-sm">Cancel</AlertDialogCancel>
            <AlertDialogAction
              id="confirm-delete-default-btn"
              onClick={handleDeleteDefault}
              disabled={deletingDefault}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90 text-sm gap-2"
            >
              {deletingDefault ? (
                <Spinner className="size-3.5" />
              ) : (
                <HugeiconsIcon icon={Unlink01Icon} className="size-3.5" />
              )}
              Disconnect
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
