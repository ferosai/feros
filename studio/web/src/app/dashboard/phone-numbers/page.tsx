"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  AlertCircleIcon,
  ArrowDown01Icon,
  ArrowLeft01Icon,
  Robot01Icon,
  CallInternal02Icon,
  CallRinging01Icon,
  Cancel01Icon,
  Delete02Icon,
  Download01Icon,
  FlashIcon,
  Link01Icon,
  Search01Icon,
  Unlink01Icon,
  ViewIcon,
  ViewOffIcon,
} from "@hugeicons/core-free-icons";
import { useCallback, useEffect, useState } from "react";
import {
  api,
  getErrorMessage,
  type Agent,
  type PhoneNumber,
  type ProviderNumber,
} from "@/lib/api/client";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { APP_TAGLINE } from "@/lib/constants";

import Image from "next/image";

// ── Helpers ────────────────────────────────────────────────────────

function ProviderLogo({
  provider,
  large = false,
}: {
  provider: "twilio" | "telnyx";
  large?: boolean;
}) {
  if (provider === "twilio") {
    return (
      <svg
        viewBox="0 0 64 64"
        xmlns="http://www.w3.org/2000/svg"
        aria-label="Twilio"
        className={large ? "size-8" : "size-3.5"}
      >
        <g transform="translate(0 .047) scale(.93704)" fill="#e31e26">
          <path d="M34.1 0C15.3 0 0 15.3 0 34.1s15.3 34.1 34.1 34.1C53 68.3 68.3 53 68.3 34.1S53 0 34.1 0zm0 59.3C20.3 59.3 9 48 9 34.1 9 20.3 20.3 9 34.1 9 48 9 59.3 20.3 59.3 34.1 59.3 48 48 59.3 34.1 59.3z" />
          <circle cx="42.6" cy="25.6" r="7.1" />
          <circle cx="42.6" cy="42.6" r="7.1" />
          <circle cx="25.6" cy="42.6" r="7.1" />
          <circle cx="25.6" cy="25.6" r="7.1" />
        </g>
      </svg>
    );
  }

  return (
    <span
      aria-label="Telnyx"
      className={`inline-flex items-center justify-center rounded-full bg-blue-600 font-bold text-white ${
        large ? "size-8 text-sm" : "size-3.5 text-[10px]"
      }`}
    >
      T
    </span>
  );
}

function ProviderWordmark({ provider }: { provider: "twilio" | "telnyx" }) {
  return (
    <div className="h-10 flex items-center justify-center overflow-visible">
      <Image
        src={
          provider === "twilio" ? "/brands/twilio-logo-wine.svg" : "/brands/telnyx-logo-black.png"
        }
        alt={provider === "twilio" ? "Twilio" : "Telnyx"}
        width={120}
        height={40}
        className="h-10 w-auto max-w-full object-contain"
      />
    </div>
  );
}

function ProviderBadge({ provider }: { provider: "twilio" | "telnyx" }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 text-[10px] font-semibold px-1.5 py-0.5 rounded-md tracking-wide ${
        provider === "twilio"
          ? "bg-red-50 text-red-600 border border-red-100"
          : "bg-blue-50 text-blue-600 border border-blue-100"
      }`}
    >
      <ProviderLogo provider={provider} />
      <span className="capitalize">{provider}</span>
    </span>
  );
}

function formatE164(num: string): string {
  // Format +15551234567 → +1 (555) 123-4567
  if (num.startsWith("+1") && num.length === 12) {
    const digits = num.slice(2);
    return `+1 (${digits.slice(0, 3)}) ${digits.slice(3, 6)}-${digits.slice(6)}`;
  }
  return num;
}

function normalizePhoneish(value: string): string {
  const digits = value.replace(/\D/g, "");
  if (digits.length >= 10) {
    return digits.length === 11 && digits.startsWith("1") ? digits.slice(1) : digits;
  }
  return value.trim().toLowerCase();
}

function hasDistinctFriendlyName(phoneNumber: string, friendlyName: string | null): boolean {
  if (!friendlyName) return false;
  return normalizePhoneish(phoneNumber) !== normalizePhoneish(friendlyName);
}

function StatusBadge({ assigned }: { assigned: boolean }) {
  return (
    <span
      className={`inline-flex items-center gap-1 text-[10px] font-medium px-2 py-0.5 rounded-full ${
        assigned ? "bg-success/10 text-success" : "bg-muted text-muted-foreground"
      }`}
    >
      <span
        className={`size-1.5 rounded-full inline-block ${assigned ? "bg-success" : "bg-muted-foreground/50"}`}
      />
      {assigned ? "Assigned" : "Unassigned"}
    </span>
  );
}

// ── Assign Modal ────────────────────────────────────────────────────

interface AssignModalProps {
  phoneNumber: PhoneNumber;
  agents: Agent[];
  voiceServerUrl: string;
  onClose: () => void;
  onSave: (updated: PhoneNumber) => void;
}

function AssignModal({ phoneNumber, agents, voiceServerUrl, onClose, onSave }: AssignModalProps) {
  const [agentId, setAgentId] = useState<string>(phoneNumber.agent_id ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const selectedAgent = agents.find((a) => a.id === agentId);
  const currentAgent = agents.find((a) => a.id === phoneNumber.agent_id);
  const actionLabel = phoneNumber.agent_id ? "Reassign Number" : "Assign Number";

  function closeModal(nextOpen: boolean) {
    if (!nextOpen) onClose();
  }

  async function handleSave() {
    if (!agentId) {
      setError("Please select an agent.");
      return;
    }
    setSaving(true);
    setError("");
    try {
      const updated = await api.phoneNumbers.assign(phoneNumber.id, {
        agent_id: agentId,
      });
      onSave(updated);
    } catch (e) {
      setError(getErrorMessage(e, "Failed to assign number"));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog open onOpenChange={closeModal}>
      <DialogContent className="max-h-[min(90vh,720px)] overflow-x-hidden overflow-y-auto rounded-2xl border-border custom-scrollbar sm:max-w-[480px]">
        <DialogHeader>
          <DialogTitle className="text-sm font-semibold">{actionLabel}</DialogTitle>
          <DialogDescription className="text-sm text-muted-foreground">
            Choose which agent should answer calls for this number.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-5 py-1">
          <div className="rounded-xl border border-border bg-secondary/40 p-4">
            <div className="flex items-center gap-2 flex-wrap">
              <span className="text-sm font-semibold text-foreground font-mono tracking-wide">
                {formatE164(phoneNumber.phone_number)}
              </span>
              <ProviderBadge provider={phoneNumber.provider} />
              <StatusBadge assigned={!!phoneNumber.agent_id} />
            </div>
            <p className="mt-2 text-xs text-muted-foreground">
              {currentAgent ? (
                <>
                  Currently assigned to{" "}
                  <span className="font-medium text-foreground">{currentAgent.name}</span>.
                </>
              ) : (
                "This number is not assigned to an agent yet."
              )}
            </p>
          </div>

          <div className="space-y-2">
            <label className="text-xs font-medium text-foreground flex items-center gap-1.5">
              <HugeiconsIcon icon={Robot01Icon} className="size-3.5 text-primary" /> Agent
            </label>
            <div className="relative">
              <select
                value={agentId}
                onChange={(e) => setAgentId(e.target.value)}
                className="w-full h-10 appearance-none rounded-lg border border-border bg-secondary/50 pl-3 pr-8 text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-ring"
              >
                <option value="">Select an agent…</option>
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name}
                  </option>
                ))}
              </select>
              <HugeiconsIcon
                icon={ArrowDown01Icon}
                className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground"
              />
            </div>
            {selectedAgent && (
              <p className="pl-1 text-[10px] text-muted-foreground">
                Status: <span className="capitalize text-foreground">{selectedAgent.status}</span>
              </p>
            )}
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-foreground flex items-center gap-1.5">
              <HugeiconsIcon icon={FlashIcon} className="size-3.5 text-primary" /> Voice Server URL
            </label>
            <div className="rounded-lg border border-border bg-secondary/50 px-3 py-2 font-mono text-xs">
              <span
                className={
                  voiceServerUrl ? "break-all text-foreground/80" : "text-muted-foreground"
                }
              >
                {voiceServerUrl || "Not configured"}
              </span>
            </div>
          </div>

          {error && (
            <div className="flex items-start gap-2 rounded-lg border border-destructive/20 bg-destructive/8 px-3 py-2">
              <HugeiconsIcon
                icon={AlertCircleIcon}
                className="mt-0.5 size-4 shrink-0 text-destructive"
              />
              <p className="min-w-0 flex-1 max-h-32 overflow-auto whitespace-pre-wrap break-all text-xs leading-relaxed text-destructive custom-scrollbar">
                {error}
              </p>
            </div>
          )}
        </div>

        <DialogFooter className="gap-2">
          <Button variant="ghost" onClick={onClose} disabled={saving} className="text-sm">
            Cancel
          </Button>
          <Button onClick={handleSave} disabled={saving} className="text-sm gap-2">
            {saving ? (
              <>
                <Spinner className="size-3.5" />
                {phoneNumber.agent_id ? "Reassigning…" : "Assigning…"}
              </>
            ) : (
              <>
                <HugeiconsIcon icon={Link01Icon} className="size-3.5" />
                {actionLabel}
              </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── Import Modal (Two-Step: Credentials → Select Numbers) ───────────

interface ImportModalProps {
  onClose: () => void;
  onImported: (numbers: PhoneNumber[]) => void;
}

function ImportModal({ onClose, onImported }: ImportModalProps) {
  // Step 1 state — credentials
  const [provider, setProvider] = useState<"twilio" | "telnyx">("twilio");
  const [twilioAccountSid, setTwilioAccountSid] = useState("");
  const [twilioAuthToken, setTwilioAuthToken] = useState("");
  const [showAuthToken, setShowAuthToken] = useState(false);
  const [telnyxApiKey, setTelnyxApiKey] = useState("");
  const [showApiKey, setShowApiKey] = useState(false);

  // Step 2 state — number selection
  const [step, setStep] = useState<1 | 2>(1);
  const [fetchedNumbers, setFetchedNumbers] = useState<ProviderNumber[]>([]);
  const [selectedNumbers, setSelectedNumbers] = useState<Set<string>>(new Set());

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  async function handleFetch() {
    setLoading(true);
    setError("");
    try {
      const result = await api.phoneNumbers.fetch({
        provider,
        ...(provider === "twilio"
          ? {
              twilio_account_sid: twilioAccountSid,
              twilio_auth_token: twilioAuthToken,
            }
          : { telnyx_api_key: telnyxApiKey }),
      });
      setFetchedNumbers(result.numbers);
      // Pre-select all importable numbers
      const importable = result.numbers
        .filter((n) => !n.already_imported)
        .map((n) => n.phone_number);
      setSelectedNumbers(new Set(importable));
      setStep(2);
    } catch (e) {
      setError(getErrorMessage(e, "Failed to fetch numbers"));
    } finally {
      setLoading(false);
    }
  }

  async function handleImport() {
    if (selectedNumbers.size === 0) return;
    setLoading(true);
    setError("");
    try {
      const result = await api.phoneNumbers.importSelected({
        provider,
        ...(provider === "twilio"
          ? {
              twilio_account_sid: twilioAccountSid,
              twilio_auth_token: twilioAuthToken,
            }
          : { telnyx_api_key: telnyxApiKey }),
        selected_numbers: Array.from(selectedNumbers),
      });
      onImported(result.phone_numbers);
    } catch (e) {
      setError(getErrorMessage(e, "Import failed"));
    } finally {
      setLoading(false);
    }
  }

  function toggleNumber(e164: string) {
    setSelectedNumbers((prev) => {
      const next = new Set(prev);
      if (next.has(e164)) next.delete(e164);
      else next.add(e164);
      return next;
    });
  }

  const canFetch =
    provider === "twilio"
      ? twilioAccountSid.length > 0 && twilioAuthToken.length > 0
      : telnyxApiKey.length > 0;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 backdrop-blur-sm">
      <div
        className="bg-card border border-border rounded-2xl shadow-2xl w-full max-w-lg mx-4 overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div>
            <h3 className="text-sm font-semibold text-foreground">Import Numbers</h3>
            <p className="text-xs text-muted-foreground mt-0.5">
              {step === 1
                ? "Enter your provider credentials"
                : `${fetchedNumbers.length} numbers found`}
            </p>
          </div>
          <button
            onClick={onClose}
            className="size-8 rounded-lg flex items-center justify-center text-muted-foreground hover:bg-secondary hover:text-foreground transition-colors"
          >
            <HugeiconsIcon icon={Cancel01Icon} className="size-4" />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-5 space-y-4">
          {step === 1 ? (
            <>
              {/* Provider tabs */}
              <div className="grid grid-cols-2 gap-2">
                {(["twilio", "telnyx"] as const).map((p) => (
                  <button
                    key={p}
                    onClick={() => setProvider(p)}
                    className={`flex items-center justify-center rounded-xl border-2 p-4 transition-all ${
                      provider === p
                        ? "border-primary bg-primary/5"
                        : "border-border bg-secondary/50 hover:border-border hover:bg-secondary"
                    }`}
                  >
                    <ProviderWordmark provider={p} />
                  </button>
                ))}
              </div>

              {/* Credential inputs */}
              {provider === "twilio" ? (
                <div className="space-y-3">
                  <div className="space-y-1.5">
                    <label className="text-xs font-medium text-foreground">Account SID</label>
                    <input
                      value={twilioAccountSid}
                      onChange={(e) => setTwilioAccountSid(e.target.value)}
                      placeholder="ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
                      className="w-full h-9 rounded-lg border border-border bg-secondary px-3 text-sm text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
                    />
                  </div>
                  <div className="space-y-1.5">
                    <label className="text-xs font-medium text-foreground">Auth Token</label>
                    <div className="relative">
                      <input
                        type={showAuthToken ? "text" : "password"}
                        value={twilioAuthToken}
                        onChange={(e) => setTwilioAuthToken(e.target.value)}
                        placeholder="Your Twilio auth token"
                        className="w-full h-9 rounded-lg border border-border bg-secondary px-3 pr-10 text-sm text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
                      />
                      <button
                        onClick={() => setShowAuthToken(!showAuthToken)}
                        className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground transition-colors"
                      >
                        {showAuthToken ? (
                          <HugeiconsIcon icon={ViewOffIcon} className="size-3.5" />
                        ) : (
                          <HugeiconsIcon icon={ViewIcon} className="size-3.5" />
                        )}
                      </button>
                    </div>
                  </div>
                </div>
              ) : (
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-foreground">API Key</label>
                  <div className="relative">
                    <input
                      type={showApiKey ? "text" : "password"}
                      value={telnyxApiKey}
                      onChange={(e) => setTelnyxApiKey(e.target.value)}
                      placeholder="Your Telnyx API key"
                      className="w-full h-9 rounded-lg border border-border bg-secondary px-3 pr-10 text-sm text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
                    />
                    <button
                      onClick={() => setShowApiKey(!showApiKey)}
                      className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground transition-colors"
                    >
                      {showApiKey ? (
                        <HugeiconsIcon icon={ViewOffIcon} className="size-3.5" />
                      ) : (
                        <HugeiconsIcon icon={ViewIcon} className="size-3.5" />
                      )}
                    </button>
                  </div>
                </div>
              )}
            </>
          ) : (
            /* Step 2: Number selection */
            <div className="space-y-2 max-h-72 overflow-y-auto custom-scrollbar pr-1">
              {fetchedNumbers.map((n) => {
                const disabled = n.already_imported;
                const checked = selectedNumbers.has(n.phone_number);
                return (
                  <button
                    key={n.phone_number}
                    onClick={() => !disabled && toggleNumber(n.phone_number)}
                    disabled={disabled}
                    className={`w-full flex items-start gap-3 rounded-lg border px-3 py-2.5 text-left transition-all ${
                      disabled
                        ? "border-border bg-secondary/30 opacity-50 cursor-not-allowed"
                        : checked
                          ? "border-primary bg-primary/5"
                          : "border-border bg-secondary/50 hover:bg-secondary"
                    }`}
                  >
                    <div
                      className={`size-4 rounded border flex items-center justify-center shrink-0 ${
                        disabled
                          ? "border-muted-foreground/30 bg-muted"
                          : checked
                            ? "border-primary bg-primary"
                            : "border-border bg-card"
                      }`}
                    >
                      {(checked || disabled) && (
                        <span className="text-[10px] text-primary-foreground font-bold">
                          {disabled ? "—" : checked ? "✓" : ""}
                        </span>
                      )}
                    </div>
                    <div className="flex-1 min-w-0">
                      <p className="text-sm font-mono font-medium text-foreground">
                        {formatE164(n.phone_number)}
                      </p>
                      {disabled && (
                        <p className="mt-1 text-[10px] text-muted-foreground leading-tight">
                          {n.disabled_reason}
                        </p>
                      )}
                    </div>
                    <span
                      className={`inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[10px] font-medium ${
                        disabled
                          ? "bg-muted text-muted-foreground"
                          : checked
                            ? "bg-primary text-primary-foreground"
                            : "border border-border bg-card text-muted-foreground"
                      }`}
                    >
                      {disabled ? "Imported" : checked ? "Selected" : "Ready"}
                    </span>
                  </button>
                );
              })}
              {fetchedNumbers.length === 0 && (
                <p className="text-sm text-muted-foreground text-center py-4">
                  No numbers found in this account.
                </p>
              )}
            </div>
          )}

          {error && (
            <div className="flex items-center gap-2 rounded-lg bg-destructive/8 border border-destructive/20 px-3 py-2">
              <HugeiconsIcon icon={AlertCircleIcon} className="size-4 text-destructive shrink-0" />
              <p className="text-xs text-destructive leading-tight">{error}</p>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-6 py-4 border-t border-border bg-secondary/40">
          <div>
            {step === 2 && (
              <span className="text-xs text-muted-foreground">{selectedNumbers.size} selected</span>
            )}
          </div>
          <div className="flex items-center gap-2">
            {step === 2 && (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setStep(1);
                  setError("");
                }}
                className="h-8 text-xs gap-1.5"
              >
                <HugeiconsIcon icon={ArrowLeft01Icon} className="size-3.5" />
                Back
              </Button>
            )}
            <Button variant="ghost" size="sm" onClick={onClose} className="h-8 text-xs">
              Cancel
            </Button>
            {step === 1 ? (
              <Button
                size="sm"
                onClick={handleFetch}
                disabled={loading || !canFetch}
                className="h-8 text-xs gap-1.5"
              >
                {loading ? (
                  <Spinner className="size-3.5" />
                ) : (
                  <HugeiconsIcon icon={Search01Icon} className="size-3.5" />
                )}
                {loading ? "Fetching…" : "Fetch Numbers"}
              </Button>
            ) : (
              <Button
                size="sm"
                onClick={handleImport}
                disabled={loading || selectedNumbers.size === 0}
                className="h-8 text-xs gap-1.5"
              >
                {loading ? (
                  <Spinner className="size-3.5" />
                ) : (
                  <HugeiconsIcon icon={Download01Icon} className="size-3.5" />
                )}
                {loading
                  ? "Importing…"
                  : `Import ${selectedNumbers.size} Number${selectedNumbers.size !== 1 ? "s" : ""}`}
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ── Phone Number Card ────────────────────────────────────────────────

interface PhoneNumberCardProps {
  num: PhoneNumber;
  agentName: string | null;
  onAssign: () => void;
  onUnassign: () => void;
  onDelete: () => void;
  busy: boolean;
}

function PhoneNumberCard({
  num,
  agentName,
  onAssign,
  onUnassign,
  onDelete,
  busy,
}: PhoneNumberCardProps) {
  const isAssigned = !!num.agent_id;
  const assignedLabel = agentName ?? num.agent_id ?? "No agent assigned";

  return (
    <div className="group flat-card p-4 transition-all duration-200 hover:shadow-md">
      <div className="flex flex-col gap-4 md:flex-row md:items-start md:justify-between">
        <div className="flex min-w-0 items-start gap-3">
          <div
            className={`flex size-10 shrink-0 items-center justify-center rounded-xl ${
              isAssigned ? "bg-primary/10" : "bg-secondary"
            }`}
          >
            <HugeiconsIcon
              icon={CallRinging01Icon}
              className={`size-5 ${isAssigned ? "text-primary" : "text-muted-foreground"}`}
            />
          </div>

          <div className="min-w-0 flex-1 space-y-3">
            <div>
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-sm font-semibold text-foreground font-mono tracking-wide">
                  {formatE164(num.phone_number)}
                </span>
                <ProviderBadge provider={num.provider} />
                <StatusBadge assigned={isAssigned} />
              </div>

              {hasDistinctFriendlyName(num.phone_number, num.friendly_name) && (
                <p className="mt-0.5 text-xs text-muted-foreground">{num.friendly_name}</p>
              )}
            </div>

            <div
              className={`rounded-xl border px-3.5 py-3 ${
                isAssigned ? "border-primary/15 bg-primary/5" : "border-border bg-secondary/50"
              }`}
            >
              <div className="flex items-start gap-3">
                <div
                  className={`mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg ${
                    isAssigned
                      ? "bg-primary text-primary-foreground"
                      : "bg-card text-muted-foreground border border-border"
                  }`}
                >
                  <HugeiconsIcon icon={Robot01Icon} className="size-4" />
                </div>
                <div className="min-w-0">
                  <p className="text-[10px] font-semibold uppercase tracking-[0.16em] text-muted-foreground">
                    {isAssigned ? "Assigned Agent" : "Agent"}
                  </p>
                  <p
                    className={`mt-1 truncate text-sm font-semibold ${
                      isAssigned ? "text-foreground" : "text-muted-foreground"
                    }`}
                  >
                    {assignedLabel}
                  </p>
                  {isAssigned && num.voice_server_url && (
                    <div className="mt-2 flex items-center gap-1.5 text-[10px] text-muted-foreground">
                      <HugeiconsIcon icon={FlashIcon} className="size-3 shrink-0" />
                      <span className="truncate font-mono">{num.voice_server_url}</span>
                    </div>
                  )}
                </div>
              </div>
            </div>
          </div>
        </div>

        <div className="flex shrink-0 flex-wrap items-center gap-2 md:justify-end">
          {isAssigned ? (
            <>
              <button
                onClick={onAssign}
                title="Reassign to different agent"
                disabled={busy}
                className="h-8 rounded-lg border border-border bg-secondary px-2.5 text-xs font-medium text-foreground transition-colors hover:bg-accent disabled:opacity-50"
              >
                <span className="flex items-center gap-1.5">
                  <HugeiconsIcon icon={Link01Icon} className="size-3.5" />
                  Reassign
                </span>
              </button>
              <button
                onClick={onUnassign}
                title="Unassign"
                disabled={busy}
                className="h-8 rounded-lg border border-border bg-secondary px-2.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-destructive/8 hover:text-destructive disabled:opacity-50"
              >
                <span className="flex items-center gap-1.5">
                  {busy ? (
                    <Spinner className="size-3.5" />
                  ) : (
                    <HugeiconsIcon icon={Unlink01Icon} className="size-3.5" />
                  )}
                  Unassign
                </span>
              </button>
            </>
          ) : (
            <button
              onClick={onAssign}
              title="Assign to agent"
              disabled={busy}
              className="h-8 rounded-lg bg-primary px-2.5 text-xs font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-50"
            >
              <span className="flex items-center gap-1.5">
                <HugeiconsIcon icon={Link01Icon} className="size-3.5" />
                Assign
              </span>
            </button>
          )}
          <button
            onClick={onDelete}
            title={`Remove from ${APP_TAGLINE}`}
            disabled={busy}
            className="flex size-8 items-center justify-center rounded-lg border border-border text-muted-foreground transition-colors hover:bg-destructive/8 hover:text-destructive disabled:opacity-50"
          >
            <HugeiconsIcon icon={Delete02Icon} className="size-3.5" />
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Page ────────────────────────────────────────────────────────────

type FilterProvider = "all" | "twilio" | "telnyx";
type FilterAssigned = "all" | "assigned" | "unassigned";

export default function PhoneNumbersPage() {
  const [numbers, setNumbers] = useState<PhoneNumber[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [loading, setLoading] = useState(true);
  const [filterProvider, setFilterProvider] = useState<FilterProvider>("all");
  const [filterAssigned, setFilterAssigned] = useState<FilterAssigned>("all");
  const [search, setSearch] = useState("");

  const [showImport, setShowImport] = useState(false);
  const [voiceServerUrl, setVoiceServerUrl] = useState("");
  const [assignTarget, setAssignTarget] = useState<PhoneNumber | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<PhoneNumber | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [numsData, agentData] = await Promise.all([api.phoneNumbers.list(), api.agents.list()]);
      setNumbers(numsData.phone_numbers);
      setAgents(agentData.agents);
    } catch {
      // silently ignore — empty state handles it
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
    api.settings
      .getTelephony()
      .then((data) => setVoiceServerUrl(data.voice_server_url ?? ""))
      .catch(() => {});
  }, [load]);

  // Agent lookup map
  const agentMap = new Map(agents.map((a) => [a.id, a.name]));

  // Filtered list
  const filtered = numbers.filter((n) => {
    if (filterProvider !== "all" && n.provider !== filterProvider) return false;
    if (filterAssigned === "assigned" && !n.agent_id) return false;
    if (filterAssigned === "unassigned" && n.agent_id) return false;
    if (search) {
      const q = search.toLowerCase();
      return (
        n.phone_number.includes(q) ||
        (n.friendly_name ?? "").toLowerCase().includes(q) ||
        (agentMap.get(n.agent_id ?? "") ?? "").toLowerCase().includes(q)
      );
    }
    return true;
  });

  async function handleUnassign(num: PhoneNumber) {
    setBusyId(num.id);
    try {
      const updated = await api.phoneNumbers.assign(num.id, { agent_id: null });
      setNumbers((prev) => prev.map((n) => (n.id === updated.id ? updated : n)));
      toast.success(`Unassigned ${formatE164(num.phone_number)}`);
    } catch (e) {
      toast.error(getErrorMessage(e, "Unassign failed"));
    } finally {
      setBusyId(null);
    }
  }

  async function handleDelete(num: PhoneNumber) {
    setDeleteTarget(num);
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const num = deleteTarget;
    setDeleteTarget(null);
    setBusyId(num.id);
    try {
      await api.phoneNumbers.delete(num.id);
      setNumbers((prev) => prev.filter((n) => n.id !== num.id));
      toast.success(`Removed ${formatE164(num.phone_number)}`);
    } catch (e) {
      toast.error(getErrorMessage(e, "Delete failed"));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-start justify-between gap-3">
        <PageHeader
          icon={CallInternal02Icon}
          title="Phone Numbers"
          description="Manage your Twilio and Telnyx numbers and route them to agents"
        />
        <div className="flex items-center gap-2 shrink-0">
          <Button size="sm" onClick={() => setShowImport(true)} className="h-8 text-xs gap-1.5">
            <HugeiconsIcon icon={Download01Icon} className="size-3.5" />
            Import Numbers
          </Button>
        </div>
      </div>

      {/* Filters */}
      <div className="flex items-center gap-3 flex-wrap">
        {/* Search */}
        <div className="relative group">
          <HugeiconsIcon
            icon={Search01Icon}
            className="absolute left-3 top-1/2 -translate-y-1/2 size-3.5 text-muted-foreground group-focus-within:text-primary transition-colors"
          />
          <input
            type="text"
            placeholder="Search numbers, agents…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="h-8 w-52 rounded-lg bg-secondary pl-8 pr-3 text-xs focus:bg-card focus:outline-none focus:ring-2 focus:ring-ring placeholder:text-muted-foreground transition-all"
          />
        </div>

        <div className="relative">
          <select
            value={filterProvider}
            onChange={(e) => setFilterProvider(e.target.value as FilterProvider)}
            className="h-8 rounded-lg bg-secondary border border-border px-3 pr-8 text-xs text-foreground focus:outline-none focus:ring-2 focus:ring-ring appearance-none cursor-pointer"
          >
            <option value="all">All providers</option>
            <option value="twilio">Twilio</option>
            <option value="telnyx">Telnyx</option>
          </select>
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 size-3 text-muted-foreground"
          />
        </div>

        <div className="relative">
          <select
            value={filterAssigned}
            onChange={(e) => setFilterAssigned(e.target.value as FilterAssigned)}
            className="h-8 rounded-lg bg-secondary border border-border px-3 pr-8 text-xs text-foreground focus:outline-none focus:ring-2 focus:ring-ring appearance-none cursor-pointer"
          >
            <option value="all">All statuses</option>
            <option value="assigned">Assigned</option>
            <option value="unassigned">Unassigned</option>
          </select>
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 size-3 text-muted-foreground"
          />
        </div>

        {numbers.length > 0 && (
          <span className="text-xs text-muted-foreground ml-auto">
            {filtered.length} of {numbers.length} numbers
          </span>
        )}
      </div>

      {/* List */}
      {loading ? (
        <div className="space-y-2">
          {[1, 2, 3].map((i) => (
            <div key={i} className="h-20 rounded-xl bg-secondary animate-pulse" />
          ))}
        </div>
      ) : numbers.length === 0 ? (
        /* ── No numbers at all ── */
        <div className="py-20 text-center">
          <div className="inline-flex items-center justify-center size-14 rounded-2xl bg-primary/8 mb-4">
            <HugeiconsIcon icon={CallRinging01Icon} className="size-6 text-primary" />
          </div>
          <p className="text-sm font-medium text-foreground mb-1">No phone numbers yet</p>
          <p className="text-sm text-muted-foreground max-w-[320px] mx-auto mb-6">
            Import existing numbers from your Twilio or Telnyx account to get started.
          </p>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => setShowImport(true)}
            className="h-8 text-xs gap-1.5"
          >
            <HugeiconsIcon icon={Download01Icon} className="size-3.5" /> Import Numbers
          </Button>
        </div>
      ) : filtered.length === 0 ? (
        /* ── Numbers exist but filters hide them ── */
        <div className="py-16 text-center">
          <p className="text-sm font-medium text-foreground mb-1">No numbers match your filters</p>
          <p className="text-sm text-muted-foreground">Try adjusting your search or filters.</p>
        </div>
      ) : (
        <div className="space-y-2">
          {filtered.map((num) => (
            <PhoneNumberCard
              key={num.id}
              num={num}
              agentName={agentMap.get(num.agent_id ?? "") ?? null}
              onAssign={() => setAssignTarget(num)}
              onUnassign={() => handleUnassign(num)}
              onDelete={() => handleDelete(num)}
              busy={busyId === num.id}
            />
          ))}
        </div>
      )}

      {/* Modals */}
      {showImport && (
        <ImportModal
          onClose={() => setShowImport(false)}
          onImported={(imported) => {
            setNumbers((prev) => {
              const map = new Map(prev.map((n) => [n.id, n]));
              imported.forEach((n) => map.set(n.id, n));
              return Array.from(map.values());
            });
            setShowImport(false);
            toast.success(`Imported ${imported.length} number(s)`);
          }}
        />
      )}

      {assignTarget && (
        <AssignModal
          phoneNumber={assignTarget}
          agents={agents}
          voiceServerUrl={voiceServerUrl}
          onClose={() => setAssignTarget(null)}
          onSave={(updated) => {
            setNumbers((prev) => prev.map((n) => (n.id === updated.id ? updated : n)));
            setAssignTarget(null);
            toast.success(`${formatE164(updated.phone_number)} assigned successfully`);
          }}
        />
      )}

      {/* Delete confirmation dialog */}
      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove phone number?</AlertDialogTitle>
            <AlertDialogDescription>
              {deleteTarget && (
                <>
                  <span className="font-mono font-medium">
                    {formatE164(deleteTarget.phone_number)}
                  </span>{" "}
                  will be removed from {APP_TAGLINE}. The number stays in your{" "}
                  {deleteTarget.provider} account — this only removes it from your workspace.
                </>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDelete}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              Remove
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
