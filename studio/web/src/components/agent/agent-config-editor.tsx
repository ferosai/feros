"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  AiVoiceGeneratorIcon,
  Alert02Icon,
  ArrowDown01Icon,
  Cancel01Icon,
  ChatBotIcon,
  CodeIcon,
  Copy01Icon,
  HierarchyIcon,
  LanguageCircleIcon,
  LinkSquare01Icon,
  PlayIcon,
  Search01Icon,
  Settings03Icon,
  Tick01Icon,
  TimeZoneIcon,
  ToolsIcon,
  Wrench01Icon,
} from "@hugeicons/core-free-icons";
import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import {
  api,
  type Agent,
  type VoiceProviderSettings,
  type VoiceOption,
  type TtsModelSpec,
} from "@/lib/api/client";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Dialog, DialogContent, DialogTrigger } from "@/components/ui/dialog";
import ConfigViewer from "@/components/agent/config-viewer";
import ConfigDiff from "@/components/agent/config-diff";
import ShikiCodeBlock from "@/components/ui/shiki-code-block";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { toast } from "sonner";

// ── Language & timezone options ──────────────────────────────────

/** Shown immediately while the /api/agents/languages fetch is in-flight. */
const LANGUAGE_OPTIONS_FALLBACK = [
  { value: "en", label: "English" },
  { value: "es", label: "Spanish" },
  { value: "fr", label: "French" },
  { value: "de", label: "German" },
  { value: "pt", label: "Portuguese" },
  { value: "it", label: "Italian" },
  { value: "ja", label: "Japanese" },
  { value: "ko", label: "Korean" },
  { value: "zh", label: "Chinese" },
  { value: "ar", label: "Arabic" },
  { value: "hi", label: "Hindi" },
  { value: "nl", label: "Dutch" },
  { value: "ru", label: "Russian" },
  { value: "pl", label: "Polish" },
  { value: "sv", label: "Swedish" },
];

type LanguageOption = { value: string; label: string };

const TIMEZONE_OPTIONS = [
  { value: "", label: "Not set" },
  { value: "America/New_York", label: "Eastern (US)" },
  { value: "America/Chicago", label: "Central (US)" },
  { value: "America/Denver", label: "Mountain (US)" },
  { value: "America/Los_Angeles", label: "Pacific (US)" },
  { value: "America/Anchorage", label: "Alaska" },
  { value: "Pacific/Honolulu", label: "Hawaii" },
  { value: "America/Sao_Paulo", label: "São Paulo" },
  { value: "Europe/London", label: "London (GMT)" },
  { value: "Europe/Paris", label: "Paris (CET)" },
  { value: "Europe/Berlin", label: "Berlin (CET)" },
  { value: "Europe/Moscow", label: "Moscow" },
  { value: "Asia/Dubai", label: "Dubai (GST)" },
  { value: "Asia/Kolkata", label: "India (IST)" },
  { value: "Asia/Shanghai", label: "China (CST)" },
  { value: "Asia/Tokyo", label: "Tokyo (JST)" },
  { value: "Asia/Seoul", label: "Seoul (KST)" },
  { value: "Australia/Sydney", label: "Sydney (AEST)" },
] as const;

// Gemini Live's built-in prebuilt voices.
// Voice names sourced from the Gemini API docs (prebuilt_voice_config.voice_name).
// This is a subset — Google documents 30 total. Preview all voices at:
// https://aistudio.google.com/apps/bundled/voice-library
const GEMINI_LIVE_VOICES = [
  { value: "Puck" },
  { value: "Charon" },
  { value: "Kore" },
  { value: "Fenrir" },
  { value: "Aoede" },
  { value: "Leda" },
  { value: "Orus" },
  { value: "Zephyr" },
  { value: "Schedar" },
  { value: "Gacrux" },
  { value: "Pulcherrima" },
  { value: "Achird" },
  { value: "Zubenelgenubi" },
  { value: "Vindemiatrix" },
  { value: "Sadachbia" },
  { value: "Sulafat" },
  { value: "Algieba" },
  { value: "Despina" },
  { value: "Erinome" },
  { value: "Algenib" },
  { value: "Rasalghul" },
  { value: "Laomedeia" },
  { value: "Achernar" },
  { value: "Alnilam" },
  { value: "Enceladus" },
  { value: "Iocaste" },
  { value: "Umbriel" },
  { value: "Oberon" },
  { value: "Callirrhoe" },
  { value: "Autonoe" },
] as const;

// ── Helpers ──────────────────────────────────────────────────────

function getConfigFields(config: Record<string, unknown>): {
  language: string;
  timezone: string;
  voice_id: string;
  tts_provider: string;
  tts_model: string;
  gemini_live_model: string;
  escalation_destinations?: { name: string; phone_number: string }[];
} {
  return {
    language: (config?.language as string) || "en",
    timezone: (config?.timezone as string) || "",
    voice_id: (config?.voice_id as string) || "",
    tts_provider: (config?.tts_provider as string) || "",
    tts_model: (config?.tts_model as string) || "",
    gemini_live_model:
      (config?.geminiLiveModel as string) || (config?.gemini_live_model as string) || "",
    escalation_destinations: config?.escalation_destinations as {
      name: string;
      phone_number: string;
    }[],
  };
}

/** Pretty provider label from the TTS settings supported_providers list. */
function providerLabel(tts: VoiceProviderSettings | null, agentProvider?: string): string {
  if (!tts) return "TTS Provider";
  const activeProvider = agentProvider || tts.provider;
  const match = tts.supported_providers.find((p) => p.value === activeProvider);
  return match?.label ?? activeProvider ?? "TTS Provider";
}

/** Docs URL for the active TTS provider (for the voice ID help link). */
function providerDocsUrl(tts: VoiceProviderSettings | null, agentProvider?: string): string | null {
  if (!tts) return null;
  const activeProvider = agentProvider || tts.provider;
  const match = tts.supported_providers.find((p) => p.value === activeProvider);
  return match?.docs_url || null;
}

/** Provider-level default voice_id. */
function providerDefaultVoiceId(tts: VoiceProviderSettings | null, agentProvider?: string): string {
  if (!tts) return "";
  const activeProvider = agentProvider || tts.provider;
  // Check agent-level provider-specific config first if it were in tts.all_configs
  return tts.all_configs?.[activeProvider]?.voice_id ?? "";
}

interface ConfigNode {
  system_prompt?: string;
  greeting?: string;
}

interface ConfigTool {
  description?: string;
  side_effect?: boolean;
  script?: string;
  summarize_result?: boolean;
}

interface FullConfig extends Record<string, unknown> {
  entry?: string;
  nodes?: Record<string, ConfigNode>;
  tools?: Record<string, ConfigTool>;
}

// ── Component ────────────────────────────────────────────────────

interface AgentConfigEditorProps {
  agentId: string;
  agent: Agent | null;
  onUpdate: (agent: Agent) => void;
  lastDiff?: string | null;
}

export default function AgentConfigEditor({
  agentId,
  agent,
  onUpdate,
  lastDiff,
}: AgentConfigEditorProps) {
  const config = agent?.current_config as Record<string, unknown> | null;
  const fields = config ? getConfigFields(config) : null;

  const [language, setLanguage] = useState(fields?.language ?? "en");
  const [timezone, setTimezone] = useState(fields?.timezone ?? "");
  const [voiceIdDraft, setVoiceIdDraft] = useState(fields?.voice_id ?? "");
  const [escalationDestinations, setEscalationDestinations] = useState<
    { id: string; name: string; phone_number: string }[]
  >(
    (fields?.escalation_destinations ?? []).map((d: { name: string; phone_number: string }) => ({
      ...d,
      id: Math.random().toString(36).substring(7),
    }))
  );
  const [saving, setSaving] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [tts, setTts] = useState<VoiceProviderSettings | null>(null);
  const [languageOptions, setLanguageOptions] = useState<LanguageOption[]>(
    LANGUAGE_OPTIONS_FALLBACK.map((o) => ({ value: o.value, label: o.label }))
  );
  const [voices, setVoices] = useState<VoiceOption[]>([]);
  const [voicesLoading, setVoicesLoading] = useState(false);
  const [ttsModels, setTtsModels] = useState<TtsModelSpec[]>([]);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [view, setView] = useState<"agent" | "prompt_tools" | "raw">("agent");
  const [copied, setCopied] = useState(false);
  const fullConfig = (config || {}) as FullConfig;
  const nodeIds = useMemo(() => Object.keys(fullConfig.nodes || {}), [fullConfig.nodes]);
  const [selectedNodeId, setSelectedNodeId] = useState<string>(
    fullConfig.entry || nodeIds[0] || ""
  );
  const [nativeMultimodalEnabled, setNativeMultimodalEnabled] = useState(false);
  const globalSyncToastId = `agent-config-sync-${agentId}`;

  // Fetch voice catalog — called on mount and whenever language changes.
  // For non-English languages, the backend hits /v1/shared-voices with
  // required_languages so the list is filtered to voices that support that language.
  const fetchVoices = useCallback(
    (lang: string) => {
      const f = config ? getConfigFields(config) : null;
      setVoicesLoading(true);
      api.settings
        .getTtsVoices(lang, f?.tts_provider || undefined)
        .then(setVoices)
        .catch(() => setVoices([]))
        .finally(() => setVoicesLoading(false));
    },
    [config]
  );

  // Load TTS provider settings + language list + voice catalog once
  useEffect(() => {
    api.settings
      .getTTS()
      .then(setTts)
      .catch(() => {});
    api.settings
      .getNativeMultimodal()
      .then((res) => setNativeMultimodalEnabled(res.has_api_key))
      .catch(() => {});
    api.agents
      .getLanguages()
      .then((langs) => setLanguageOptions(langs.map((l) => ({ value: l.code, label: l.label }))))
      .catch(() => {}); // keep fallback list on error
    // Fetch TTS model catalog for language→voice suggestions
    api.agents
      .getTtsModels()
      .then(setTtsModels)
      .catch(() => {});
  }, []);

  // Initial voice fetch (once we know the agent language from config)
  const [voicesFetchedForLang, setVoicesFetchedForLang] = useState<string | null>(null);
  useEffect(() => {
    if (!config) return;
    const f = getConfigFields(config);
    // Only fetch once per language — language changes are handled by the onChange handler
    if (voicesFetchedForLang === f.language) return;
    setVoicesFetchedForLang(f.language);
    fetchVoices(f.language);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [config]);

  // Sync draft states when fields change from outside (e.g., initial load or full sync)
  const escalationDestinationsJson = JSON.stringify(fields?.escalation_destinations);
  useEffect(() => {
    if (!fields) return;
    setLanguage(fields.language);
    setTimezone(fields.timezone);
    setVoiceIdDraft(fields.voice_id);
    setEscalationDestinations(
      (fields.escalation_destinations ?? []).map((d: { name: string; phone_number: string }) => ({
        ...d,
        id: Math.random().toString(36).substring(7),
      }))
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    fields?.language,
    fields?.timezone,
    fields?.voice_id,
    fields?.gemini_live_model,
    escalationDestinationsJson,
  ]);

  const patchField = useCallback(
    async (payload: Record<string, unknown>) => {
      const primaryField = Object.keys(payload).find((k) => k !== "regenerate_greeting");
      const shouldRegen = Boolean(payload["regenerate_greeting"]);
      if (!primaryField && !shouldRegen) return;
      const trackField = primaryField ?? "language";
      const showGlobalSyncToast = !["voice_id", "language", "timezone"].includes(trackField);

      setSaving(trackField);
      setSaved(null);
      if (showGlobalSyncToast) {
        toast.loading("Syncing configuration...", { id: globalSyncToastId });
      }
      try {
        const updated = await api.agents.patchConfig(agentId, payload);
        onUpdate(updated);
        setSaved(trackField);
        setTimeout(() => setSaved(null), 1500);
        if (updated.greeting_updated && updated.greeting) {
          toast.success("Greeting synced", {
            description: updated.greeting,
          });
        }
        if (updated.model_warning) {
          toast.warning("Voice model warning", {
            description: updated.model_warning,
          });
        }
      } catch {
        toast.error("Failed to update configuration");
      } finally {
        setSaving(null);
        if (showGlobalSyncToast) {
          toast.dismiss(globalSyncToastId);
        }
      }
    },
    [agentId, globalSyncToastId, onUpdate]
  );

  // Debounced save for voice_id text input
  const handleVoiceIdChange = useCallback(
    (value: string) => {
      setVoiceIdDraft(value);
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        const payload: Record<string, string> = { voice_id: value };

        // Use the agent's current provider if set, otherwise fallback to global
        const f = config ? getConfigFields(config) : null;
        const activeProvider = f?.tts_provider || tts?.provider;
        const activeModel =
          f?.tts_model || (activeProvider === tts?.provider ? tts?.config?.model : "");

        if (activeProvider) {
          payload.tts_provider = activeProvider;
          if (activeModel) {
            payload.tts_model = activeModel as string;
          }
        }
        patchField(payload);
      }, 600);
    },
    [patchField, tts, config]
  );

  // Debounced save for Gemini Voice, avoids clobbering TTS provider/model
  const handleGeminiVoiceIdChange = useCallback(
    (value: string) => {
      setVoiceIdDraft(value);
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        patchField({ voice_id: value });
      }, 600);
    },
    [patchField]
  );

  // Switch the agent's TTS provider — saves provider + model, clears voice, re-fetches voices
  const handleProviderChange = useCallback(
    async (newProvider: string) => {
      if (!tts) return;
      const savedConfig = tts.all_configs?.[newProvider] || {};
      const defaultModel = savedConfig.model || "";
      // Clear the voice — user should pick from the new provider's list
      setVoiceIdDraft("");
      const payload: Record<string, string> = {
        tts_provider: newProvider,
        voice_id: "",
        tts_model: defaultModel || "",
      };
      await patchField(payload);
      api.settings
        .getTtsVoices(language, newProvider)
        .then(setVoices)
        .catch(() => setVoices([]));
    },
    [tts, patchField, language]
  );

  if (!config) return null;

  const configData = fullConfig;
  const fallbackNodeId = configData.entry || nodeIds[0] || "";
  const activeNodeId = configData.nodes?.[selectedNodeId] ? selectedNodeId : fallbackNodeId;
  const selectedNode = configData.nodes?.[activeNodeId];
  const systemPrompt = selectedNode?.system_prompt;
  const greeting = selectedNode?.greeting;
  const tools = Object.entries(configData.tools || {});
  const f = getConfigFields(config);
  const providerName = providerLabel(tts, f.tts_provider);
  const docsUrl = providerDocsUrl(tts, f.tts_provider);
  const defaultVoiceId = providerDefaultVoiceId(tts, f.tts_provider);
  // When Gemini Live is active, TTS is entirely bypassed.
  // Voice selection goes through Gemini's own prebuilt voices instead.
  const isGeminiLive = !!f.gemini_live_model.trim();
  const timezoneOptions = TIMEZONE_OPTIONS.some((o) => o.value === timezone)
    ? TIMEZONE_OPTIONS
    : [{ value: timezone, label: `${timezone} (Custom)` }, ...TIMEZONE_OPTIONS];
  const handleCopy = () => {
    navigator.clipboard.writeText(JSON.stringify(configData, null, 2));
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between px-1">
        <div className="flex items-center gap-1 p-1 rounded-xl bg-accent/20 border border-border/40">
          <button
            onClick={() => setView("agent")}
            className={cn(
              "flex items-center gap-2 px-4 py-1.5 rounded-lg text-[10px] font-bold transition-all duration-300 select-none outline-none",
              view === "agent"
                ? "bg-background text-foreground shadow-sm ring-1 ring-border/20"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            <HugeiconsIcon icon={Settings03Icon} className="size-3.5" />
            Agent Config
          </button>
          <button
            onClick={() => setView("prompt_tools")}
            className={cn(
              "flex items-center gap-2 px-4 py-1.5 rounded-lg text-[10px] font-bold transition-all duration-300 select-none outline-none",
              view === "prompt_tools"
                ? "bg-background text-foreground shadow-sm ring-1 ring-border/20"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            <HugeiconsIcon icon={ChatBotIcon} className="size-3.5" />
            Prompt & Tools
          </button>
          <button
            onClick={() => setView("raw")}
            className={cn(
              "flex items-center gap-2 px-4 py-1.5 rounded-lg text-[10px] font-bold transition-all duration-300 select-none outline-none",
              view === "raw"
                ? "bg-background text-foreground shadow-sm ring-1 ring-border/20"
                : "text-muted-foreground hover:text-foreground"
            )}
          >
            <HugeiconsIcon icon={CodeIcon} className="size-3.5" />
            Raw JSON
          </button>
        </div>

        {view === "raw" && (
          <button
            onClick={handleCopy}
            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-[10px] font-bold text-muted-foreground hover:text-primary hover:bg-primary/5 transition-all active:scale-95 border border-transparent hover:border-primary/10"
          >
            <HugeiconsIcon
              icon={copied ? Tick01Icon : Copy01Icon}
              className={cn("size-4", copied && "text-emerald-500")}
            />
            {copied ? "Copied" : "Copy JSON"}
          </button>
        )}
      </div>

      {lastDiff && <ConfigDiff description={lastDiff} />}

      <div className="relative">
        <div
          className={cn(
            "transition-all duration-500",
            view === "agent"
              ? "opacity-100 translate-y-0"
              : "opacity-0 translate-y-4 pointer-events-none absolute w-full"
          )}
        >
          {view === "agent" && (
            <div className="space-y-6">
              <div className="relative group/container">
                <div className="absolute -inset-4 bg-linear-to-tr from-primary/5 via-transparent to-primary/5 rounded-[2rem] blur-2xl opacity-0 group-hover/container:opacity-100 transition-opacity duration-1000 -z-10" />

                <div className="rounded-2xl border border-border bg-background shadow-[0_8px_40px_rgba(0,0,0,0.04)]">
                  <div className="divide-y divide-border/50">
                    <SettingRow
                      icon={<HugeiconsIcon icon={LanguageCircleIcon} className="size-4" />}
                      label="Spoken Language"
                      description="The primary language your agent speaks and understands"
                    >
                      <SelectField
                        value={language}
                        options={languageOptions}
                        saving={saving === "language"}
                        saved={saved === "language"}
                        onChange={(v) => {
                          setLanguage(v);
                          setVoiceIdDraft("");
                          patchField({
                            language: v,
                            voice_id: "",
                            regenerate_greeting: true,
                          });

                          fetchVoices(v);
                          setVoicesFetchedForLang(v);
                        }}
                      />
                    </SettingRow>

                    <SettingRow
                      icon={<HugeiconsIcon icon={TimeZoneIcon} className="size-4" />}
                      label="Agent Timezone"
                      description="Used for date-relative prompts and scheduling"
                    >
                      <SelectField
                        value={timezone}
                        options={timezoneOptions}
                        saving={saving === "timezone"}
                        saved={saved === "timezone"}
                        onChange={(v) => {
                          setTimezone(v);
                          patchField({ timezone: v });
                        }}
                      />
                    </SettingRow>

                    {/* ── Voice Settings ───────────────────────────────────────────────────────
                        When Gemini Live is active, TTS is bypassed entirely. Show Gemini's
                        built-in prebuilt voice picker instead of the TTS provider controls.
                    */}
                    {isGeminiLive ? (
                      <SettingRow
                        icon={<HugeiconsIcon icon={AiVoiceGeneratorIcon} className="size-4" />}
                        label="Gemini Voice"
                        description="Gemini Live's built-in prebuilt voice — replaces TTS while native audio is active"
                      >
                        <div className="flex items-center gap-2">
                          <Select
                            value={
                              GEMINI_LIVE_VOICES.some((v) => v.value === voiceIdDraft)
                                ? voiceIdDraft
                                : GEMINI_LIVE_VOICES[0].value
                            }
                            onValueChange={(v) => handleGeminiVoiceIdChange(v)}
                            disabled={saving === "voice_id"}
                          >
                            <SelectTrigger className="h-9 w-auto min-w-36 text-xs font-bold px-3 border-border/60 bg-accent/30 rounded-lg hover:bg-accent/50 transition-colors">
                              <SelectValue>
                                {GEMINI_LIVE_VOICES.some((v) => v.value === voiceIdDraft)
                                  ? voiceIdDraft
                                  : GEMINI_LIVE_VOICES[0].value}
                              </SelectValue>
                            </SelectTrigger>
                            <SelectContent>
                              {GEMINI_LIVE_VOICES.map((v) => (
                                <SelectItem
                                  key={v.value}
                                  value={v.value}
                                  className="text-xs font-semibold"
                                >
                                  {v.value}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                          <div className="flex items-center w-6 justify-center">
                            {saving === "voice_id" && (
                              <Spinner className="size-3.5 text-primary animate-spin" />
                            )}
                            {saved === "voice_id" && (
                              <HugeiconsIcon
                                icon={Tick01Icon}
                                className="size-4 text-emerald-500"
                              />
                            )}
                          </div>
                        </div>
                      </SettingRow>
                    ) : (
                      <SettingRow
                        icon={<HugeiconsIcon icon={AiVoiceGeneratorIcon} className="size-4" />}
                        label="Voice Settings"
                        description="Configure the provider, model, and vocal identity for synthetic speech"
                        className="items-start py-6 flex-col gap-4"
                        childrenClassName="w-full ml-0"
                      >
                        <div className="flex w-full items-center gap-3 pl-12 flex-wrap">
                          <div className="flex items-center gap-2">
                            {tts &&
                              (() => {
                                const configuredProviders = tts.supported_providers.filter(
                                  (p) => tts.all_credentials?.[p.value]
                                );
                                if (configuredProviders.length > 1) {
                                  return (
                                    <Select
                                      value={f.tts_provider || tts.provider}
                                      onValueChange={handleProviderChange}
                                      disabled={saving === "tts_provider"}
                                    >
                                      <SelectTrigger className="h-9 w-auto min-w-32 text-xs font-bold px-3 border-border/60 bg-accent/30 rounded-lg hover:bg-accent/50 transition-colors">
                                        <SelectValue>{providerName}</SelectValue>
                                      </SelectTrigger>
                                      <SelectContent>
                                        {configuredProviders.map((p) => (
                                          <SelectItem
                                            key={p.value}
                                            value={p.value}
                                            className="text-xs font-semibold"
                                          >
                                            {p.label}
                                          </SelectItem>
                                        ))}
                                      </SelectContent>
                                    </Select>
                                  );
                                }
                                return (
                                  <span className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-[10px] font-bold bg-accent/50 border border-border/50 text-muted-foreground uppercase tracking-wider">
                                    {providerName}
                                    {docsUrl && (
                                      <a
                                        href={docsUrl}
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="text-muted-foreground hover:text-primary transition-colors"
                                        onClick={(e) => e.stopPropagation()}
                                      >
                                        <HugeiconsIcon icon={LinkSquare01Icon} className="size-3" />
                                      </a>
                                    )}
                                  </span>
                                );
                              })()}

                            {ttsModels &&
                              tts &&
                              (() => {
                                const activeProvider = f.tts_provider || tts.provider;
                                const providerModels = ttsModels.filter(
                                  (m) => m.provider === activeProvider
                                );
                                if (providerModels.length > 0) {
                                  let activeModel =
                                    f.tts_model ||
                                    (activeProvider === tts.provider ? tts.config?.model : "") ||
                                    providerModels[0].model_id;
                                  if (!providerModels.some((m) => m.model_id === activeModel)) {
                                    activeModel = providerModels[0].model_id;
                                  }
                                  return (
                                    <Select
                                      value={activeModel}
                                      onValueChange={(m) => patchField({ tts_model: m })}
                                      disabled={saving === "tts_model"}
                                    >
                                      <SelectTrigger className="h-9 w-auto min-w-28 text-xs font-bold px-3 border-border/60 bg-accent/30 rounded-lg hover:bg-accent/50 transition-colors">
                                        <SelectValue>
                                          {providerModels.find((m) => m.model_id === activeModel)
                                            ?.label ?? activeModel}
                                        </SelectValue>
                                      </SelectTrigger>
                                      <SelectContent>
                                        {providerModels.map((m) => (
                                          <SelectItem
                                            key={m.model_id}
                                            value={m.model_id}
                                            className="text-xs font-semibold"
                                          >
                                            {m.label || m.model_id}
                                          </SelectItem>
                                        ))}
                                      </SelectContent>
                                    </Select>
                                  );
                                }
                                return null;
                              })()}
                          </div>

                          <div className="flex items-center gap-3">
                            {voicesLoading ? (
                              <div className="h-9 w-52 flex items-center justify-center bg-accent/20 rounded-lg border border-dashed border-border/60">
                                <Spinner className="size-4 text-muted-foreground/60" />
                              </div>
                            ) : voices.length > 0 ? (
                              <VoicePicker
                                voices={voices}
                                value={voiceIdDraft}
                                onChange={(id) => {
                                  handleVoiceIdChange(id);
                                }}
                                disabled={saving === "voice_id"}
                              />
                            ) : (
                              <div className="relative flex-1">
                                <input
                                  type="text"
                                  value={voiceIdDraft}
                                  onChange={(e) => handleVoiceIdChange(e.target.value)}
                                  placeholder={
                                    defaultVoiceId
                                      ? `default (${truncate(defaultVoiceId, 16)})`
                                      : "custom-voice-id"
                                  }
                                  disabled={saving === "voice_id"}
                                  className="h-9 w-full rounded-lg border border-border/60 bg-accent/30 px-3 text-xs font-bold text-foreground placeholder:text-muted-foreground/40 focus:outline-none focus:ring-1 focus:ring-primary/40 disabled:opacity-50 transition-all font-mono"
                                />
                              </div>
                            )}

                            <div className="flex items-center gap-1">
                              {voiceIdDraft && (
                                <button
                                  onClick={() => {
                                    navigator.clipboard.writeText(voiceIdDraft);
                                    setSaved("voice_id_copy");
                                    setTimeout(() => setSaved(null), 1500);
                                  }}
                                  title="Copy Voice ID"
                                  className="p-2 rounded-lg hover:bg-accent/60 text-muted-foreground hover:text-primary transition-all active:scale-95"
                                >
                                  <HugeiconsIcon
                                    icon={saved === "voice_id_copy" ? Tick01Icon : Copy01Icon}
                                    className={`size-4 ${saved === "voice_id_copy" ? "text-emerald-500" : ""}`}
                                  />
                                </button>
                              )}
                              <div className="flex items-center w-6 justify-center">
                                {saving === "voice_id" && (
                                  <Spinner className="size-3.5 text-primary animate-spin" />
                                )}
                                {saved === "voice_id" && (
                                  <HugeiconsIcon
                                    icon={Tick01Icon}
                                    className="size-4 text-emerald-500"
                                  />
                                )}
                              </div>
                            </div>
                          </div>
                        </div>
                      </SettingRow>
                    )}

                    <SettingRow
                      icon={<HugeiconsIcon icon={ChatBotIcon} className="size-4" />}
                      label="Conversation Mode"
                      description="Choose between the standard Builder LLM pipeline or native bidirectional audio"
                      className="items-start py-6 flex-col gap-4"
                      childrenClassName="w-full ml-0 pl-12"
                    >
                      <Select
                        value={isGeminiLive ? "native" : "standard"}
                        onValueChange={(val) => {
                          const newModel = val === "native" ? "default" : "";
                          const payload: Record<string, string> = {
                            gemini_live_model: newModel,
                          };

                          if (val === "native") {
                            if (!GEMINI_LIVE_VOICES.some((v) => v.value === voiceIdDraft)) {
                              payload.voice_id = GEMINI_LIVE_VOICES[0].value;
                              setVoiceIdDraft(GEMINI_LIVE_VOICES[0].value);
                            }
                          } else {
                            if (GEMINI_LIVE_VOICES.some((v) => v.value === voiceIdDraft)) {
                              payload.voice_id = "";
                              setVoiceIdDraft("");
                            }
                          }

                          patchField(payload);
                        }}
                      >
                        <SelectTrigger className="h-9 w-[280px] text-xs font-bold px-3 border-border/60 bg-accent/30 rounded-lg hover:bg-accent/50 transition-colors">
                          <SelectValue>
                            {isGeminiLive
                              ? "Native Multimodal (Gemini Live)"
                              : "Standard Pipeline (STT + LLM + TTS)"}
                          </SelectValue>
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="standard" className="text-xs font-semibold">
                            Standard Pipeline (STT + LLM + TTS)
                          </SelectItem>
                          {(isGeminiLive || nativeMultimodalEnabled) && (
                            <SelectItem value="native" className="text-xs font-semibold">
                              Native Multimodal (Gemini Live)
                            </SelectItem>
                          )}
                        </SelectContent>
                      </Select>
                    </SettingRow>

                    <SettingRow
                      icon={<HugeiconsIcon icon={Alert02Icon} className="size-4" />}
                      label="Human Handoff / Escalation"
                      description="Configure escalation destinations for telephony agents"
                      className="items-start py-6 flex-col gap-4"
                      childrenClassName="w-full ml-0 pl-12"
                    >
                      <div className="space-y-3">
                        {escalationDestinations.map((dest, idx) => (
                          <div key={dest.id} className="flex items-center gap-2">
                            <input
                              type="text"
                              value={dest.name}
                              onChange={(e) => {
                                const newDests = [...escalationDestinations];
                                newDests[idx].name = e.target.value;
                                setEscalationDestinations(newDests);
                              }}
                              onBlur={() => {
                                const valid = escalationDestinations.filter(
                                  (d) => d.name.trim() !== "" && d.phone_number.trim() !== ""
                                );
                                patchField({
                                  escalation_destinations: valid.map(({ id: _, ...rest }) => rest),
                                });
                              }}
                              placeholder="Name (e.g. Sales)"
                              disabled={saving === "escalation_destinations"}
                              className="h-9 w-40 rounded-lg border border-border/60 bg-accent/30 px-3 text-xs font-bold text-foreground placeholder:text-muted-foreground/40 focus:outline-none focus:ring-1 focus:ring-primary/40 disabled:opacity-50 transition-all"
                            />
                            <input
                              type="text"
                              value={dest.phone_number}
                              onChange={(e) => {
                                const newDests = [...escalationDestinations];
                                newDests[idx].phone_number = e.target.value;
                                setEscalationDestinations(newDests);
                              }}
                              onBlur={() => {
                                const valid = escalationDestinations.filter(
                                  (d) => d.name.trim() !== "" && d.phone_number.trim() !== ""
                                );
                                patchField({
                                  escalation_destinations: valid.map(({ id: _, ...rest }) => rest),
                                });
                              }}
                              placeholder="Phone (e.g. +1234567890)"
                              disabled={saving === "escalation_destinations"}
                              className="h-9 w-48 rounded-lg border border-border/60 bg-accent/30 px-3 text-xs font-bold text-foreground placeholder:text-muted-foreground/40 focus:outline-none focus:ring-1 focus:ring-primary/40 disabled:opacity-50 transition-all font-mono"
                            />
                            <button
                              onClick={() => {
                                const newDests = escalationDestinations.filter((_, i) => i !== idx);
                                setEscalationDestinations(newDests);
                                const valid = newDests.filter(
                                  (d) => d.name.trim() !== "" && d.phone_number.trim() !== ""
                                );
                                patchField({
                                  escalation_destinations: valid.map(({ id: _, ...rest }) => rest),
                                });
                              }}
                              className="p-2 text-muted-foreground hover:text-red-500 transition-colors rounded-lg hover:bg-red-500/10"
                            >
                              <HugeiconsIcon icon={Cancel01Icon} className="size-4" />
                            </button>
                          </div>
                        ))}
                        <button
                          onClick={() => {
                            const newDests = [
                              ...escalationDestinations,
                              {
                                id: Math.random().toString(36).substring(7),
                                name: "",
                                phone_number: "",
                              },
                            ];
                            setEscalationDestinations(newDests);
                          }}
                          className="flex items-center justify-center gap-2 px-4 py-2 mt-1 rounded-lg text-xs font-bold text-primary bg-primary/10 hover:bg-primary/20 border border-primary/20 hover:border-primary/40 transition-all active:scale-[0.98]"
                        >
                          + Add Destination
                        </button>
                      </div>
                    </SettingRow>
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>

        <div
          className={cn(
            "transition-all duration-500",
            view === "prompt_tools"
              ? "opacity-100 translate-y-0"
              : "opacity-0 translate-y-4 pointer-events-none absolute top-0 left-0 w-full"
          )}
        >
          {view === "prompt_tools" && (
            <div className="space-y-6">
              <section className="relative group/container group/section">
                <div className="absolute -inset-4 bg-linear-to-tr from-primary/5 via-transparent to-primary/5 rounded-[2rem] blur-2xl opacity-0 group-hover/container:opacity-100 transition-opacity duration-1000 -z-10" />

                <div className="rounded-2xl border border-border bg-background shadow-[0_8px_40px_rgba(0,0,0,0.04)] overflow-hidden">
                  <div className="flex flex-col gap-4 px-5 py-4 border-b border-border/50 md:flex-row md:items-center md:justify-between">
                    <div className="flex items-center gap-3 min-w-0">
                      <div className="size-8 rounded-lg bg-accent/30 flex items-center justify-center text-muted-foreground shrink-0 group-hover/section:bg-primary/10 group-hover/section:text-primary transition-all duration-300">
                        <HugeiconsIcon icon={ChatBotIcon} className="size-4" />
                      </div>
                      <div className="min-w-0">
                        <h3 className="text-xs font-bold tracking-tight text-foreground">
                          Persona
                        </h3>
                        <p className="text-[10px] font-medium text-muted-foreground leading-snug">
                          Prompt and greeting configuration for the selected node.
                        </p>
                      </div>
                    </div>

                    {nodeIds.length >= 1 && (
                      <Select value={activeNodeId} onValueChange={setSelectedNodeId}>
                        <SelectTrigger className="h-9 w-full md:w-auto md:min-w-[220px] max-w-[320px] gap-2 bg-accent/20 border-border/60 text-[10px] font-bold uppercase tracking-widest">
                          <HugeiconsIcon
                            icon={HierarchyIcon}
                            className="size-3.5 opacity-60 shrink-0"
                          />
                          <SelectValue>{activeNodeId.replaceAll("_", " ")}</SelectValue>
                        </SelectTrigger>
                        <SelectContent className="rounded-xl shadow-2xl">
                          {nodeIds.map((id) => (
                            <SelectItem
                              key={id}
                              value={id}
                              className="text-[10px] font-bold uppercase tracking-widest"
                            >
                              {id}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    )}
                  </div>

                  <div className="p-6 md:p-8">
                    {greeting && (
                      <div className="mb-8 border-l-2 border-primary/50 pl-4">
                        <div className="text-[10px] font-bold uppercase tracking-widest text-muted-foreground mb-1.5">
                          Agent Greeting
                        </div>
                        <div className="text-sm font-medium leading-relaxed text-foreground/90 italic">
                          &quot;{greeting}&quot;
                        </div>
                      </div>
                    )}

                    {systemPrompt ? (
                      <div className="prose prose-sm max-w-none  prose-headings:text-sm prose-headings:text-foreground prose-headings:font-bold prose-headings:tracking-tight text-muted-foreground prose-p:leading-relaxed prose-strong:text-primary prose-code:text-primary prose-code:bg-primary/5 prose-code:px-1 prose-code:rounded prose-code:font-bold prose-code:before:content-none prose-code:after:content-none prose-pre:bg-accent/30 prose-pre:border prose-pre:border-border/50">
                        <ReactMarkdown remarkPlugins={[remarkGfm]}>
                          {systemPrompt.replace(/\\n/g, "\n")}
                        </ReactMarkdown>
                      </div>
                    ) : (
                      <div className="py-20 flex flex-col items-center justify-center text-center opacity-50">
                        <HugeiconsIcon
                          icon={Alert02Icon}
                          className="size-10 mb-4 text-muted-foreground"
                        />
                        <p className="text-sm font-medium">
                          No persona prompt defined for entry node.
                        </p>
                      </div>
                    )}
                  </div>
                </div>
              </section>

              <section className="relative group/container group/section pb-8 @container">
                <div className="absolute -inset-4 bg-linear-to-tr from-primary/5 via-transparent to-primary/5 rounded-[2rem] blur-2xl opacity-0 group-hover/container:opacity-100 transition-opacity duration-1000 -z-10" />

                <div className="rounded-2xl border border-border bg-background shadow-[0_8px_40px_rgba(0,0,0,0.04)] p-5 md:p-6">
                  <div className="mb-5 flex items-center gap-3 min-w-0">
                    <div className="size-8 rounded-lg bg-accent/30 flex items-center justify-center text-muted-foreground shrink-0 group-hover/section:bg-primary/10 group-hover/section:text-primary transition-all duration-300">
                      <HugeiconsIcon icon={ToolsIcon} className="size-4" />
                    </div>
                    <div className="min-w-0">
                      <h3 className="text-xs font-bold tracking-tight text-foreground">Tools</h3>
                      <p className="text-[10px] font-medium text-muted-foreground leading-snug">
                        Runtime tools available to this agent.
                      </p>
                    </div>
                  </div>

                  {tools.length > 0 ? (
                    <div className="grid grid-cols-1 @xl:grid-cols-2 @6xl:grid-cols-3 gap-4">
                      {tools.map(([id, tool]) => (
                        <Dialog key={id}>
                          <DialogTrigger asChild>
                            <div
                              role="button"
                              tabIndex={0}
                              className="group/tool text-left cursor-pointer outline-none flex flex-col p-4 rounded-xl border border-border bg-background shadow-[0_4px_20px_rgba(0,0,0,0.03)] hover:shadow-[0_8px_30px_rgba(0,0,0,0.06)] hover:border-border/80 transition-all duration-200"
                            >
                              <div className="flex items-center gap-2 min-w-0 mb-2">
                                <div className="size-8 rounded-lg bg-accent/60 text-foreground/60 flex items-center justify-center shrink-0 ring-1 ring-border/60">
                                  <HugeiconsIcon icon={Wrench01Icon} className="size-3.5" />
                                </div>
                                <div className="flex flex-col min-w-0">
                                  <span className="text-sm font-bold text-foreground tracking-tight leading-tight truncate">
                                    {id}
                                  </span>
                                  <div className="flex items-center gap-1.5 mt-0.5 opacity-60">
                                    <span className="text-[10px] font-bold uppercase tracking-widest leading-none">
                                      {tool.side_effect ? "Write" : "Read"}
                                    </span>
                                    <div
                                      className={cn(
                                        "size-1 rounded-full",
                                        tool.side_effect ? "bg-amber-400" : "bg-emerald-400"
                                      )}
                                    />
                                  </div>
                                </div>
                              </div>

                              <div className="flex items-center gap-4 my-3">
                                <TooltipProvider>
                                  <Tooltip delayDuration={300}>
                                    <TooltipTrigger asChild>
                                      <div
                                        className="flex items-center gap-1 group"
                                        onClick={(e) => e.stopPropagation()}
                                        onPointerDown={(e) => e.stopPropagation()}
                                      >
                                        <Switch
                                          className="scale-70"
                                          id={`tool-summarize-${id}`}
                                          checked={!!tool.summarize_result}
                                          onCheckedChange={(checked) =>
                                            patchField({
                                              tool_summarize_overrides: {
                                                [id]: checked || null,
                                              },
                                            })
                                          }
                                        />
                                        <label
                                          htmlFor={`tool-summarize-${id}`}
                                         className="text-[10px] font-bold text-foreground/50 uppercase tracking-wide group-hover:text-foreground">
                                          Summarize Result
                                        </label>
                                      </div>
                                    </TooltipTrigger>
                                    <TooltipContent className="text-[10px] font-bold py-2 px-2">
                                      Use LLM to summarize long tool outputs
                                    </TooltipContent>
                                  </Tooltip>

                                  <Tooltip delayDuration={300}>
                                    <TooltipTrigger asChild>
                                      <button className="inline-flex gap-1.5 group text-[10px] font-bold uppercase tracking-wide text-foreground/50">
                                        <HugeiconsIcon icon={CodeIcon} className="size-3.5" />
                                        <span className="group-hover:text-foreground">Source Code</span>
                                      </button>
                                    </TooltipTrigger>
                                    <TooltipContent className="text-[10px] font-bold py-2 px-2">
                                      View Tool Implementation
                                    </TooltipContent>
                                  </Tooltip>
                                </TooltipProvider>
                              </div>

                              {/* Row 3: description */}
                              <p className="text-xs text-foreground/60 leading-relaxed font-medium line-clamp-2 italic">
                                {tool.description || "No description provided."}
                              </p>
                            </div>
                          </DialogTrigger>
                          <DialogContent className="sm:max-w-[980px] w-[90vw] p-0 overflow-hidden rounded-3xl border-border bg-background shadow-2xl">
                            <div className="flex items-center justify-between px-6 py-4 border-b border-border/60 bg-accent/20">
                              <div className="flex items-center gap-3">
                                <div className="size-8 rounded-lg bg-accent/60 flex items-center justify-center text-muted-foreground">
                                  <HugeiconsIcon icon={CodeIcon} className="size-4" />
                                </div>
                                <div className="flex flex-col gap-0.5">
                                  <span className="text-sm font-bold tracking-tight break-all">
                                    {id}
                                  </span>
                                </div>
                              </div>
                            </div>
                            <div className="p-0 bg-[#ffffff] overflow-auto max-h-[70vh]">
                              <ShikiCodeBlock
                                code={
                                  tool.script?.replace(/\\n/g, "\n") ||
                                  "// No implementation provided"
                                }
                                lang="javascript"
                                className="max-h-[70vh]"
                              />
                            </div>
                          </DialogContent>
                        </Dialog>
                      ))}
                    </div>
                  ) : (
                    <div className="py-12 flex flex-col items-center justify-center text-center opacity-50">
                      <HugeiconsIcon
                        icon={Alert02Icon}
                        className="size-10 mb-4 text-muted-foreground"
                      />
                      <p className="text-sm font-medium">No tools configured.</p>
                    </div>
                  )}
                </div>
              </section>
            </div>
          )}
        </div>

        <div
          className={cn(
            "transition-all duration-500",
            view === "raw"
              ? "opacity-100 translate-y-0"
              : "opacity-0 translate-y-4 pointer-events-none absolute top-0 left-0 w-full"
          )}
        >
          {view === "raw" && <ConfigViewer config={configData} />}
        </div>
      </div>
    </div>
  );
}

// ── Sub-components ───────────────────────────────────────────────

function truncate(s: string, n: number) {
  return s.length > n ? s.slice(0, n) + "…" : s;
}

function SettingRow({
  icon,
  label,
  description,
  children,
  className,
  childrenClassName,
}: {
  icon: React.ReactNode;
  label: string;
  description: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  childrenClassName?: string;
}) {
  return (
    <div
      className={cn(
        "flex items-center justify-between px-5 py-4 bg-transparent transition-all hover:bg-accent/2 group/row first:rounded-t-2xl last:rounded-b-2xl",
        className
      )}
    >
      <div className="flex items-center gap-4 min-w-0 flex-1 w-full">
        <div className="size-8 rounded-lg bg-accent/30 flex items-center justify-center text-muted-foreground shrink-0 group-hover/row:bg-primary/10 group-hover/row:text-primary transition-all duration-300">
          {icon}
        </div>
        <div className="min-w-0 flex flex-col gap-0.5">
          <div className="text-xs font-bold tracking-tight text-foreground">{label}</div>
          <div className="text-[10px] text-muted-foreground leading-snug font-medium">
            {description}
          </div>
        </div>
      </div>
      <div className={cn("shrink-0 ml-4", childrenClassName)}>{children}</div>
    </div>
  );
}

function SelectField({
  value,
  options,
  saving,
  saved,
  onChange,
}: {
  value: string;
  options: { value: string; label: string }[] | readonly { value: string; label: string }[];
  saving: boolean;
  saved: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <div className="relative">
        <Select value={value} onValueChange={onChange} disabled={saving}>
          <SelectTrigger className="h-9 min-w-[140px] truncate rounded-lg border-border/60 bg-accent/30 px-3 text-xs font-bold text-foreground focus:ring-1 focus:ring-primary/40 hover:bg-accent/50 transition-all border">
            <SelectValue>{options.find((o) => o.value === value)?.label ?? value}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            {options
              .filter((o) => o.value !== "")
              .map((o) => (
                <SelectItem key={o.value} value={o.value} className="text-xs font-semibold">
                  {o.label}
                </SelectItem>
              ))}
          </SelectContent>
        </Select>
      </div>
      <div className="flex w-4 justify-center items-center shrink-0">
        {saving && <Spinner className="size-3.5 text-primary" />}
        {saved && <HugeiconsIcon icon={Tick01Icon} className="size-4 text-emerald-500" />}
      </div>
    </div>
  );
}

// ── VoicePicker ───────────────────────────────────────────────────
// Searchable combobox that shows voice name, gender, and a play
// button for audio preview.  Renders as a styled button that opens
// a floating dropdown; closes on outside click or Escape.

function VoicePicker({
  voices,
  value,
  onChange,
  disabled,
}: {
  voices: VoiceOption[];
  value: string;
  onChange: (id: string) => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  const selected = voices.find((v) => v.voice_id === value);

  // Close on outside click / Escape
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onDown = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [open]);

  // Focus search when opening
  useEffect(() => {
    if (open) setTimeout(() => searchRef.current?.focus(), 50);
  }, [open]);

  // Reset query when closing
  useEffect(() => {
    if (!open) {
      const t = setTimeout(() => setQuery(""), 50);
      return () => clearTimeout(t);
    }
  }, [open]);

  const filtered = query.trim()
    ? voices.filter(
        (v) =>
          v.name.toLowerCase().includes(query.toLowerCase()) ||
          v.gender.toLowerCase().includes(query.toLowerCase()) ||
          v.description.toLowerCase().includes(query.toLowerCase())
      )
    : voices;

  const playPreview = (url: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!url) return;
    audioRef.current?.pause();
    audioRef.current = new Audio(url);
    audioRef.current.play().catch(() => {});
  };

  const VoiceRow = ({ v }: { v: VoiceOption }) => (
    <button
      key={v.voice_id}
      type="button"
      onClick={() => {
        onChange(v.voice_id);
        setOpen(false);
      }}
      className={`w-full flex items-center justify-between gap-2 px-3 py-2 text-left text-[10px] hover:bg-accent transition-colors ${
        v.voice_id === value ? "bg-primary/5 text-primary" : "text-foreground"
      }`}
    >
      <div className="min-w-0">
        <div className="font-medium truncate">{v.name}</div>
        {v.description && (
          <div className="text-[10px] text-muted-foreground truncate">{v.description}</div>
        )}
      </div>
      <div className="flex items-center gap-1 shrink-0">
        {v.language && (
          <span className="text-[10px] px-1 py-0.5 rounded bg-accent border border-border text-muted-foreground uppercase tracking-wide">
            {v.language.slice(0, 2)}
          </span>
        )}
        {v.gender && (
          <span className="text-[10px] px-1 py-0.5 rounded bg-accent border border-border text-muted-foreground capitalize">
            {v.gender}
          </span>
        )}
        {v.preview_url && (
          <button
            type="button"
            onClick={(e) => playPreview(v.preview_url, e)}
            title="Preview voice"
            className="text-muted-foreground hover:text-primary transition-colors"
          >
            <HugeiconsIcon icon={PlayIcon} className="size-3" />
          </button>
        )}
      </div>
    </button>
  );

  return (
    <div ref={containerRef} className="relative">
      {/* Trigger button */}
      <button
        type="button"
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
        className="h-8 w-52 flex items-center justify-between gap-1.5 rounded-lg border border-border bg-accent/50 px-2.5 text-[10px] font-medium text-foreground hover:bg-accent focus:outline-none focus:ring-1 focus:ring-primary/30 disabled:opacity-50 transition-colors"
      >
        <span className="truncate">
          {selected ? selected.name : value ? truncate(value, 22) : "Select a voice…"}
        </span>
        <div className="flex items-center gap-1 shrink-0">
          {selected?.language && (
            <span className="text-[10px] px-1 py-0.5 rounded bg-accent border border-border text-muted-foreground uppercase tracking-wide">
              {selected.language.slice(0, 2)}
            </span>
          )}
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            className={`size-3 text-muted-foreground transition-transform ${open ? "rotate-180" : ""}`}
          />
        </div>
      </button>

      {/* Dropdown */}
      {open && (
        <div className="absolute right-0 top-full mt-1 z-50 w-72 rounded-xl border border-border bg-background shadow-2xl overflow-hidden">
          {/* Search */}
          <div className="flex items-center gap-2 px-3 py-2 border-b border-border">
            <HugeiconsIcon icon={Search01Icon} className="size-3 text-muted-foreground shrink-0" />
            <input
              ref={searchRef}
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search voices…"
              className="flex-1 bg-transparent text-[10px] text-foreground placeholder:text-muted-foreground/50 focus:outline-none"
            />
            {query && (
              <button
                onClick={() => setQuery("")}
                className="text-muted-foreground hover:text-foreground"
              >
                <HugeiconsIcon icon={Cancel01Icon} className="size-3" />
              </button>
            )}
          </div>

          {/* Voice list */}
          <div className="max-h-64 overflow-y-auto">
            {filtered.length === 0 ? (
              <div className="px-3 py-4 text-center text-[10px] text-muted-foreground">
                No voices match &ldquo;{query}&rdquo;
              </div>
            ) : (
              filtered.map((v) => <VoiceRow key={v.voice_id} v={v} />)
            )}
          </div>
        </div>
      )}
    </div>
  );
}
