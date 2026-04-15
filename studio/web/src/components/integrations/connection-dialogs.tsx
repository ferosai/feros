"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  Alert01Icon,
  AlertCircleIcon,
  CheckmarkCircle02Icon,
  InformationCircleIcon,
  Settings02Icon,
  ShieldUserIcon,
  SquareLock01Icon,
  ViewIcon,
  ViewOffIcon,
} from "@hugeicons/core-free-icons";
import { useEffect, useState } from "react";
import { toast } from "sonner";

import {
  api,
  type CredentialField,
  type CredentialSchema,
  type IntegrationSummary,
  type OAuthApp,
} from "@/lib/api/client";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { IntegrationIcon } from "@/components/ui/integration-icon";

type ConnectionType = "oauth" | "byok";
export type ConnectionDialogMode = "default" | "override";

interface ExistingConnectionLike {
  id?: string;
  auth_type: string;
}


function chooseDefaultConnectionType(schema: CredentialSchema, oauthReady: boolean): ConnectionType {
  const supportsOAuth = schema.auth_type === "oauth2";
  const hasByok = (schema.fields?.length ?? 0) > 0;

  if (supportsOAuth && oauthReady) return "oauth";
  if (supportsOAuth && !oauthReady && hasByok) return "byok";
  if (supportsOAuth) return "oauth";
  return "byok";
}

export interface OAuthAppRegistrationDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  integration: IntegrationSummary | null;
  existing: OAuthApp | null;
  onSaved?: () => void | Promise<void>;
}

export function OAuthAppRegistrationDialog({
  open,
  onOpenChange,
  integration,
  existing,
  onSaved,
}: OAuthAppRegistrationDialogProps) {
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [showSecret, setShowSecret] = useState(false);
  const [saving, setSaving] = useState(false);
  const [callbackUrl, setCallbackUrl] = useState<string>("{server_url}/api/oauth/callback");

  useEffect(() => {
    if (!open) return;
    setClientId(existing?.client_id ?? "");
    setClientSecret("");
    setShowSecret(false);
  }, [existing, open]);

  useEffect(() => {
    if (!open) return;
    let active = true;

    api.oauth
      .getCallbackUrl()
      .then((res) => {
        if (!active) return;
        setCallbackUrl(res.callback_url || "{server_url}/api/oauth/callback");
      })
      .catch(() => {
        if (!active) return;
        if (typeof window !== "undefined") {
          setCallbackUrl(`${window.location.origin}/api/oauth/callback`);
          return;
        }
        setCallbackUrl("{server_url}/api/oauth/callback");
      });

    return () => {
      active = false;
    };
  }, [open]);

  const handleSave = async () => {
    if (!integration) return;
    if (!clientId.trim()) {
      toast.error("Client ID is required");
      return;
    }
    if (!existing && !clientSecret.trim()) {
      toast.error("Client Secret is required for new registrations");
      return;
    }

    setSaving(true);
    try {
      if (existing) {
        const patch: { client_id?: string; client_secret?: string } = {
          client_id: clientId.trim(),
        };
        if (clientSecret.trim()) patch.client_secret = clientSecret.trim();
        await api.oauthApps.patch(integration.name, patch);
      } else {
        await api.oauthApps.upsert({
          integration_name: integration.name,
          client_id: clientId.trim(),
          client_secret: clientSecret.trim(),
          enabled: true,
        });
      }
      toast.success(
        `${integration.display_name} OAuth app ${existing ? "updated" : "registered"}`
      );
      await onSaved?.();
      onOpenChange(false);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Failed to save OAuth app");
    } finally {
      setSaving(false);
    }
  };

  if (!integration) return null;
  const isEdit = !!existing;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[480px] rounded-2xl border-border" id="register-oauth-dialog">
        <DialogHeader>
          <DialogTitle className="text-sm font-semibold">
            {isEdit ? "Update" : "Register"} {integration.display_name} OAuth App
          </DialogTitle>
          <DialogDescription className="text-sm text-muted-foreground">
            {isEdit
              ? "Update the OAuth client credentials. Leave Client Secret blank to keep the existing secret."
              : "Enter the OAuth client credentials from your developer app registration."}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-5 py-2">
          <div className="flex items-start gap-3 p-3.5 rounded-xl bg-primary/5 border border-primary/10">
            <HugeiconsIcon icon={AlertCircleIcon} className="size-3.5 text-primary mt-0.5 shrink-0" />
            <p className="text-[10px] text-muted-foreground leading-relaxed">
              Create an OAuth app in your <span className="font-medium text-foreground">{integration.display_name}</span>{" "}
              developer portal, set the callback or redirect URL to{" "}
              <span className="font-mono text-foreground">{callbackUrl}</span>, then paste the{" "}
              <span className="font-medium text-foreground">Client ID</span> and{" "}
              <span className="font-medium text-foreground">Client Secret</span> below.
            </p>
          </div>

          <div className="space-y-2">
            <label className="text-xs font-medium text-muted-foreground">Client ID</label>
            <Input
              id="oauth-client-id"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder="e.g. 1234567890abcdef"
              className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm"
            />
          </div>

          <div className="space-y-2">
            <label className="text-xs font-medium text-muted-foreground">
              Client Secret
              {isEdit && <span className="ml-1 text-muted-foreground/60">(leave blank to keep existing)</span>}
            </label>
            <div className="relative">
              <Input
                id="oauth-client-secret"
                type={showSecret ? "text" : "password"}
                value={clientSecret}
                onChange={(e) => setClientSecret(e.target.value)}
                placeholder={isEdit ? "••••••••  (unchanged)" : "paste your client secret…"}
                className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm pr-10"
              />
              <button
                type="button"
                onClick={() => setShowSecret(!showSecret)}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground transition-colors"
              >
                {showSecret ? <HugeiconsIcon icon={ViewOffIcon} className="size-3.5" /> : <HugeiconsIcon icon={ViewIcon} className="size-3.5" />}
              </button>
            </div>
            <p className="text-[10px] text-muted-foreground flex items-center gap-1">
              <HugeiconsIcon icon={SquareLock01Icon} className="size-2.5" /> Encrypted at rest with AES-256-GCM · never exposed via API
            </p>
          </div>
        </div>

        <DialogFooter className="gap-2">
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={saving} className="text-sm">
            Cancel
          </Button>
          <Button
            id="save-oauth-app-btn"
            onClick={handleSave}
            disabled={saving}
            className="text-sm bg-primary text-primary-foreground hover:bg-primary/90 gap-2"
          >
            {saving ? (
              <>
                <Spinner className="size-3.5" />
                Saving…
              </>
            ) : (
              <>
                <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-3.5" />
                {isEdit ? "Update" : "Register"}
              </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export interface IntegrationConnectionDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  integration: IntegrationSummary | null;
  existing: ExistingConnectionLike | null;
  oauthReady: boolean;
  mode: ConnectionDialogMode;
  agentId?: string;
  onSaved?: () => void;
  onOAuthStarted?: () => void;
  onConfigureOAuthApp: () => void;
}

export function IntegrationConnectionDialog({
  open,
  onOpenChange,
  integration,
  existing,
  oauthReady,
  mode,
  agentId,
  onSaved,
  onOAuthStarted,
  onConfigureOAuthApp,
}: IntegrationConnectionDialogProps) {
  const [schema, setSchema] = useState<CredentialSchema | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});
  const [shown, setShown] = useState<Record<string, boolean>>({});
  const [loadingSchema, setLoadingSchema] = useState(false);
  const [saving, setSaving] = useState(false);
  const [oauthConnecting, setOAuthConnecting] = useState(false);
  const [type, setType] = useState<ConnectionType>("oauth");

  const supportsOAuth = schema?.auth_type === "oauth2";
  const hasByokFields = (schema?.fields?.length ?? 0) > 0;
  const hasModeSelector = !!supportsOAuth && hasByokFields;

  useEffect(() => {
    if (!open || !integration) return;
    setLoadingSchema(true);
    setFields({});
    setShown({});
    setType("oauth");

    api.integrations
      .getCredentialSchema(integration.name)
      .then((s) => {
        setSchema(s);
        const modeFromExisting: ConnectionType | null = existing
          ? existing.auth_type === "oauth2"
            ? "oauth"
            : "byok"
          : null;

        if (modeFromExisting) {
          setType(modeFromExisting);
          return;
        }
        setType(chooseDefaultConnectionType(s, oauthReady));
      })
      .catch(() => toast.error("Failed to load integration schema"))
      .finally(() => setLoadingSchema(false));
  }, [open, integration, existing, oauthReady]);

  const handleOAuthConnect = async () => {
    if (!integration) return;
    if (mode === "override" && !agentId) {
      toast.error("Missing agent context for override");
      return;
    }
    setOAuthConnecting(true);
    try {
      const { authorize_url } = mode === "override"
        ? await api.oauth.authorize(integration.name, agentId as string)
        : await api.oauth.authorizeDefault(integration.name);
      const popup = window.open(authorize_url, "oauth_popup", "width=600,height=700");
      if (!popup) {
        toast.error("Popup blocked — please allow popups and try again");
        return;
      }
      onOAuthStarted?.();
      onOpenChange(false);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Failed to start OAuth flow");
    } finally {
      setOAuthConnecting(false);
    }
  };

  const handleByokSave = async () => {
    if (!integration || !schema) return;
    if (mode === "override" && !agentId) {
      toast.error("Missing agent context for override");
      return;
    }

    const editingExistingByok =
      mode === "override" &&
      !!existing &&
      existing.auth_type !== "oauth2" &&
      typeof existing.id === "string";

    const missing = schema.fields
      .filter((f: CredentialField) => f.required && !fields[f.key]?.trim() && !editingExistingByok)
      .map((f: CredentialField) => f.label);

    if (missing.length) {
      toast.error(`Required: ${missing.join(", ")}`);
      return;
    }

    setSaving(true);
    try {
      const data: Record<string, string> = {};
      for (const f of schema.fields) {
        if (fields[f.key]?.trim()) data[f.key] = fields[f.key].trim();
      }

      if (editingExistingByok && !Object.keys(data).length) {
        toast.info("No changes to save");
        onOpenChange(false);
        return;
      }

      if (mode === "override") {
        const authType = supportsOAuth ? "api_key" : integration.auth_type;
        if (editingExistingByok && existing?.id) {
          await api.credentials.update(agentId as string, existing.id, { data });
        } else {
          await api.credentials.create(agentId as string, {
            name: `${integration.display_name} Connection`,
            provider: integration.name,
            auth_type: authType,
            data,
          });
        }
        toast.success(`${integration.display_name} override ${editingExistingByok ? "updated" : "configured"}`);
      } else {
        await api.integrations.upsertDefaultConnection(integration.name, {
          auth_type: supportsOAuth ? "api_key" : integration.auth_type,
          data,
        });
        toast.success(`${integration.display_name} default connection ${existing ? "updated" : "configured"}`);
      }

      onSaved?.();
      onOpenChange(false);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Failed to save connection");
    } finally {
      setSaving(false);
    }
  };

  const openOAuthSetup = () => {
    onOpenChange(false);
    onConfigureOAuthApp();
  };

  if (!integration) return null;

  const title = mode === "override"
    ? `Override ${integration.display_name} Connection`
    : `Connect To ${integration.display_name}`;

  const description = mode === "override"
    ? "Agent-specific override · this agent will use this connection instead of platform default."
    : "Platform-wide · all agents inherit this connection by default.";

  const oauthBody = mode === "override"
    ? `Continue with OAuth to create an agent-specific override for ${integration.display_name}.`
    : `Continue with OAuth to create a platform-wide default connection for ${integration.display_name}.`;

  const oauthMissingBody = mode === "override"
    ? `No OAuth app is registered for ${integration.display_name}. Configure OAuth app first before creating this override.`
    : `No OAuth app is registered for ${integration.display_name}. Configure OAuth app first before connecting.`;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[520px] rounded-2xl border-border" id="connect-dialog">
        <DialogHeader>
          <div className="flex items-center gap-3 mb-1">
            <div className="relative size-10 rounded-xl bg-secondary flex items-center justify-center shrink-0 overflow-hidden">
              <IntegrationIcon
                name={integration.name}
                iconHint={integration.icon}
                size="size-5"
                brandSize="size-9"
              />
            </div>
            <div>
              <DialogTitle className="text-sm font-semibold">{title}</DialogTitle>
              <DialogDescription className="text-xs text-muted-foreground mt-0.5">
                {description}
              </DialogDescription>
            </div>
          </div>
        </DialogHeader>

        <div className="space-y-4 py-1">
          {loadingSchema ? (
            <div className="flex items-center justify-center py-8">
              <Spinner className="size-5 text-muted-foreground" />
            </div>
          ) : (
            <>
              {hasModeSelector && (
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">Connection Type</label>
                  <Select value={type} onValueChange={(v: ConnectionType) => setType(v)}>
                    <SelectTrigger className="h-10 rounded-lg bg-secondary/50 border-border text-xs">
                      <SelectValue>
                        {type === "oauth" ? (
                          <span className="inline-flex items-center gap-1.5">
                            OAuth
                            {!oauthReady && (
                              <HugeiconsIcon icon={Alert01Icon} className="size-3.5 text-amber-500" aria-hidden />
                            )}
                          </span>
                        ) : (
                          "BYOK"
                        )}
                      </SelectValue>
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="oauth">
                        <span className="inline-flex items-center gap-1.5">
                          OAuth
                          {!oauthReady && (
                            <HugeiconsIcon icon={Alert01Icon} className="size-3.5 text-amber-500" aria-hidden />
                          )}
                        </span>
                      </SelectItem>
                      <SelectItem value="byok">BYOK</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              )}

              {supportsOAuth && type === "oauth" && !oauthReady && (
                <div className="space-y-3">
                  <div className="flex items-start gap-3 p-3 rounded-xl bg-amber-500/5 border border-amber-500/15">
                    <HugeiconsIcon icon={AlertCircleIcon} className="size-3.5 text-amber-600 mt-0.5 shrink-0" />
                    <p className="text-[10px] text-muted-foreground leading-relaxed">
                      {oauthMissingBody}
                    </p>
                  </div>
                  <Button
                    id={`configure-oauth-app-${integration.name}`}
                    onClick={openOAuthSetup}
                    variant="outline"
                    className="w-full h-10 text-sm gap-2"
                  >
                    <HugeiconsIcon icon={Settings02Icon} className="size-3.5" />
                    Configure OAuth App
                  </Button>
                </div>
              )}

              {supportsOAuth && type === "oauth" && oauthReady && (
                <div className="space-y-3">
                  <div className="flex items-start gap-3 p-3 rounded-xl bg-primary/5 border border-primary/10">
                    <HugeiconsIcon icon={InformationCircleIcon} className="size-3.5 text-primary mt-0.5 shrink-0" />
                    <p className="text-[10px] text-muted-foreground leading-relaxed">{oauthBody}</p>
                  </div>
                  <Button
                    id={`oauth-connect-${mode}-${integration.name}`}
                    onClick={handleOAuthConnect}
                    disabled={oauthConnecting}
                    className="w-full h-10 text-sm bg-primary text-primary-foreground hover:bg-primary/90 gap-2"
                  >
                    {oauthConnecting ? (
                      <>
                        <Spinner className="size-3.5" />
                        Opening…
                      </>
                    ) : (
                      <>
                        <HugeiconsIcon icon={ShieldUserIcon} className="size-3.5" />
                        Connect with OAuth
                      </>
                    )}
                  </Button>
                  <button
                    type="button"
                    onClick={openOAuthSetup}
                    className="w-full text-xs text-muted-foreground hover:text-foreground underline-offset-2 hover:underline transition-colors flex items-center justify-center gap-1.5"
                  >
                    <HugeiconsIcon icon={Settings02Icon} className="size-3" />
                    Edit OAuth App
                  </button>
                </div>
              )}

              {((!supportsOAuth && hasByokFields) || (type === "byok" && hasByokFields)) && (
                <div className="space-y-3">
                  <div className="space-y-3">
                    {schema?.fields.map((field: CredentialField) => (
                      <div key={field.key} className="space-y-1.5">
                        <label className="text-xs font-medium text-muted-foreground flex items-center gap-1.5">
                          {field.label}
                          {!field.required && (
                            <span className="text-muted-foreground/50 font-normal">(optional)</span>
                          )}
                        </label>
                        <div className="relative">
                          <Input
                            id={`${mode}-conn-${field.key}`}
                            type={field.type === "password" && !shown[field.key] ? "password" : "text"}
                            value={fields[field.key] ?? ""}
                            onChange={(e) => setFields((prev) => ({ ...prev, [field.key]: e.target.value }))}
                            placeholder={
                              existing && field.type === "password"
                                ? "••••••••  (leave blank to keep)"
                                : field.placeholder || `Enter ${field.label}…`
                            }
                            className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm pr-10"
                          />
                          {field.type === "password" && (
                            <button
                              type="button"
                              onClick={() => setShown((prev) => ({ ...prev, [field.key]: !prev[field.key] }))}
                              className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground transition-colors"
                            >
                              {shown[field.key] ? <HugeiconsIcon icon={ViewOffIcon} className="size-3.5" /> : <HugeiconsIcon icon={ViewIcon} className="size-3.5" />}
                            </button>
                          )}
                        </div>
                        {field.help_text && (
                          <p className="text-[10px] text-muted-foreground">
                            {field.help_text}
                            {field.help_url && (
                              <>
                                {" "}
                                <a
                                  href={field.help_url}
                                  target="_blank"
                                  rel="noopener noreferrer"
                                  className="underline hover:text-foreground transition-colors"
                                >
                                  Docs ↗
                                </a>
                              </>
                            )}
                          </p>
                        )}
                      </div>
                    ))}
                    <p className="text-[10px] text-muted-foreground flex items-center gap-1">
                      <HugeiconsIcon icon={SquareLock01Icon} className="size-2.5" /> Encrypted at rest with AES-256-GCM · never exposed via API
                    </p>
                  </div>
                </div>
              )}
            </>
          )}
        </div>

        <DialogFooter className="gap-2">
          <Button variant="ghost" onClick={() => onOpenChange(false)} className="text-sm" disabled={saving || oauthConnecting}>
            Cancel
          </Button>
          {((!supportsOAuth && hasByokFields) || (type === "byok" && hasByokFields)) && (
            <Button
              id={`save-${mode}-connection-btn`}
              onClick={handleByokSave}
              disabled={saving || loadingSchema || !schema}
              className="text-sm bg-primary text-primary-foreground hover:bg-primary/90 gap-2"
            >
              {saving ? (
                <>
                  <Spinner className="size-3.5" />
                  Saving…
                </>
              ) : (
                <>
                  <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-3.5" />
                  {mode === "override" ? "Save override credentials" : "Save credentials"}
                </>
              )}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
