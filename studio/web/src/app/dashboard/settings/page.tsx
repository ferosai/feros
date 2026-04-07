"use client";

import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react";
import { Alert01Icon, AlertCircleIcon, ArrowRight01Icon, ArrowDown01Icon, ArrowUp01Icon, Call02Icon, CheckmarkCircle02Icon, CloudIcon, FlashIcon, SettingDone02Icon, LinkSquare01Icon, SquareLock01Icon, Mic01Icon, ServerStack01Icon, Settings01Icon, ViewIcon, ViewOffIcon, VolumeHighIcon, AiVoiceIcon, Analytics01Icon, AiCloudIcon, AiProgrammingIcon, AiMicIcon, LiveStreaming02Icon } from "@hugeicons/core-free-icons";
import { useState, useEffect, useCallback, useRef, type ClipboardEvent, Suspense } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { PageHeader } from "@/components/ui/page-header";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  api,
  type LLMSettings,
  type LLMSettingsUpdate,
  type LLMProviderOption,
  type VoiceProviderSettings,
  type VoiceProviderUpdate,
  type VoiceProviderOption,
  type VoiceProviderField,
  type TelephonySettingsUpdate,
  type SttModelSpec,
  type ObservabilitySettings,
  type ObservabilitySettingsUpdate,
} from "@/lib/api/client";
import { toast } from "sonner";

// ── Provider metadata ──────────────────────────────────────────

const PROVIDER_HELP: Record<string, { models: string[]; docs: string; placeholder: string }> = {
  groq: {
    models: [
      "llama-3.3-70b-versatile",
      "llama-3.1-8b-instant",
      "qwen/qwen3-32b",
      "deepseek-r1-distill-llama-70b",
    ],
    docs: "https://console.groq.com/docs/quickstart",
    placeholder: "https://api.groq.com/openai/v1",
  },
  openai: {
    models: ["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"],
    docs: "https://platform.openai.com/docs",
    placeholder: "https://api.openai.com",
  },
  anthropic: {
    models: ["claude-sonnet-4-20250514", "claude-3-5-sonnet-20241022", "claude-3-haiku-20240307"],
    docs: "https://docs.anthropic.com",
    placeholder: "https://api.anthropic.com",
  },
  gemini: {
    models: ["gemini-3-flash-preview", "gemini-2.5-flash", "gemini-2.5-flash-lite"],
    docs: "https://ai.google.dev/docs",
    placeholder: "https://generativelanguage.googleapis.com",
  },
  deepseek: {
    models: ["deepseek-chat", "deepseek-reasoner"],
    docs: "https://platform.deepseek.com/docs",
    placeholder: "https://api.deepseek.com",
  },
  ollama: {
    models: ["llama3.2", "llama3.1:70b", "mixtral:8x7b", "gemma2:9b", "qwen2.5:32b"],
    docs: "https://ollama.com/library",
    placeholder: "http://localhost:11434",
  },
  together: {
    models: [
      "meta-llama/Llama-3.1-70B-Instruct-Turbo",
      "meta-llama/Llama-3.1-8B-Instruct-Turbo",
      "mistralai/Mixtral-8x7B-Instruct-v0.1",
    ],
    docs: "https://docs.together.ai",
    placeholder: "https://api.together.xyz",
  },
  fireworks: {
    models: [
      "accounts/fireworks/models/llama-v3p1-70b-instruct",
      "accounts/fireworks/models/llama-v3p1-8b-instruct",
    ],
    docs: "https://docs.fireworks.ai",
    placeholder: "https://api.fireworks.ai/inference",
  },
  openrouter: {
    models: [
      "google/gemini-2.0-flash-001",
      "anthropic/claude-3.5-sonnet",
      "openai/gpt-4o",
      "meta-llama/llama-3.3-70b-instruct",
      "deepseek/deepseek-chat-v3-0324",
    ],
    docs: "https://openrouter.ai/docs",
    placeholder: "https://openrouter.ai/api/v1",
  },
  vllm: {
    models: [],
    docs: "https://docs.vllm.ai",
    placeholder: "http://localhost:8000",
  },
  custom: {
    models: [],
    docs: "",
    placeholder: "https://your-api.example.com",
  },
};

const DEFAULT_VOICE_SERVER_URL = "http://localhost:8300";

const SETTINGS_TABS = [
  { id: "ai-models", label: "AI Models" },
  { id: "voice-infrastructure", label: "Voice Infrastructure" },
  { id: "observability", label: "Observability" },
] as const;

type SettingsSectionId = (typeof SETTINGS_TABS)[number]["id"];

function isValidVoiceServerUrl(value: string): boolean {
  return value.startsWith("http://") || value.startsWith("https://");
}

interface OpenRouterModel {
  id: string;
  name: string;
}

// ── LLM Card Component ──────────────────────────────────────────

function LLMCard({
  title,
  subtitle,
  icon: Icon,
  llm,
  providers,
  provider,
  setProvider,
  model,
  setModel,
  baseUrl,
  setBaseUrl,
  apiKey,
  setApiKey,
  temperature,
  setTemperature,
  showApiKey,
  setShowApiKey,
  loading,
  quickStartHint,
}: {
  title: string;
  subtitle: string;
  icon: IconSvgElement;
  llm: LLMSettings | null;
  providers: LLMProviderOption[];
  provider: string;
  setProvider: (v: string) => void;
  model: string;
  setModel: (v: string) => void;
  baseUrl: string;
  setBaseUrl: (v: string) => void;
  apiKey: string;
  setApiKey: (v: string) => void;
  temperature: number;
  setTemperature: (v: number) => void;
  showApiKey: boolean;
  setShowApiKey: (v: boolean) => void;
  loading: boolean;
  quickStartHint?: boolean;
}) {
  // OpenRouter models type-ahead
  const [orModels, setOrModels] = useState<OpenRouterModel[]>([]);
  const [orQuery, setOrQuery] = useState("");
  const [orOpen, setOrOpen] = useState(false);
  const [orLoading, setOrLoading] = useState(false);
  const orRef = useRef<HTMLDivElement>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);

  // eslint-disable-next-line react-hooks/set-state-in-effect
  useEffect(() => { setSelectedIndex(0); }, [orQuery]);

  useEffect(() => {
    if (provider !== "openrouter" || orModels.length > 0) return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setOrLoading(true);
    fetch("https://openrouter.ai/api/v1/models")
      .then((r) => r.json())
      .then((data) => {
        if (cancelled) return;
        const models: OpenRouterModel[] = (data.data || [])
          .map((m: { id: string; name: string }) => ({ id: m.id, name: m.name }))
          .sort((a: OpenRouterModel, b: OpenRouterModel) => a.id.localeCompare(b.id));
        setOrModels(models);
      })
      .catch(() => {})
      .finally(() => { if (!cancelled) setOrLoading(false); });
    return () => { cancelled = true; };
  }, [provider, orModels.length]);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (orRef.current && !orRef.current.contains(e.target as Node)) {
        setOrOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, []);

  const queryParts = orQuery.toLowerCase().split(/\s+/).filter(Boolean);
  const filteredOrModels = orModels
    .filter((m) => {
      if (queryParts.length === 0) return true;
      const id = m.id.toLowerCase();
      const name = m.name.toLowerCase();
      return queryParts.every((part) => id.includes(part) || name.includes(part));
    })
    .slice(0, 30);

  const needsApiKey = provider !== "ollama";
  const needsBaseUrl = provider === "ollama" || provider === "custom" || provider === "vllm";
  const help = PROVIDER_HELP[provider] || PROVIDER_HELP.custom;

  return (
    <section className="flat-card p-6">
      <div className="flex items-center gap-3 mb-6">
        <div className="size-9 rounded-xl bg-primary/5 flex items-center justify-center">
          <HugeiconsIcon icon={Icon} className="size-4 text-primary" />
        </div>
        <div className="flex-1">
          <h3 className="text-sm font-semibold text-foreground">{title}</h3>
          <p className="text-xs text-muted-foreground mt-0.5">{subtitle}</p>
        </div>
        {llm && (
          <Badge
            className={`${
              llm.all_credentials?.[provider] || (llm.provider === provider && llm.has_api_key) || apiKey || provider === "ollama"
                ? "bg-primary/10 text-primary border-primary/20"
                : "bg-destructive/10 text-destructive border-destructive/20"
            } border shadow-none text-[10px] font-medium rounded-full`}
          >
            {llm.all_credentials?.[provider] || (llm.provider === provider && llm.has_api_key) || apiKey || provider === "ollama"
              ? "Configured"
              : "Needs API Key"}
          </Badge>
        )}
      </div>

      {loading ? (
        <div className="flex justify-center py-12">
          <Spinner className="size-5 text-muted-foreground" />
        </div>
      ) : (
        <div className="space-y-6">
          {/* Provider Select */}
          <div className="space-y-2">
            <label className="text-xs font-medium text-muted-foreground">Provider</label>
            <div className="grid grid-cols-2 lg:grid-cols-4 gap-2">
              {providers.map((p) => (
                <button
                  key={p.value}
                  onClick={() => {
                    setProvider(p.value);
                    const saved = llm?.all_configs?.[p.value];
                    if (saved) {
                      if (saved.model) setModel(saved.model);
                      if (saved.base_url) setBaseUrl(saved.base_url);
                      if (saved.temperature) setTemperature(parseFloat(saved.temperature));
                    } else {
                      const ph = PROVIDER_HELP[p.value];
                      if (ph?.models.length) setModel(ph.models[0]);
                      if (ph?.placeholder) setBaseUrl(ph.placeholder);
                    }
                  }}
                  className={`relative flex flex-col items-start gap-1 p-3 rounded-xl border transition-all text-left ${
                    provider === p.value
                      ? "border-primary bg-primary/5 ring-1 ring-primary/20"
                      : "border-border hover:border-muted-foreground/30 bg-background"
                  }`}
                >
                  <div className="flex items-center justify-between w-full">
                    <span className="text-sm font-semibold text-foreground flex items-center gap-2">
                      {(llm?.all_credentials?.[p.value] || (llm?.provider === p.value && llm?.has_api_key)) && (
                        <TooltipProvider>
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <div className="size-1.5 rounded-full bg-emerald-500 shadow-[0_0_5px_rgba(16,185,129,0.5)] shrink-0" />
                            </TooltipTrigger>
                            <TooltipContent side="top">
                              <p className="text-[10px] font-medium">Saved credentials</p>
                            </TooltipContent>
                          </Tooltip>
                        </TooltipProvider>
                      )}
                      {p.label}
                    </span>
                  </div>
                  <span className="text-[10px] text-muted-foreground leading-tight">{p.description}</span>
                </button>
              ))}
            </div>
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-5">
            {/* Model */}
            <div className="space-y-2">
              <label className="text-xs font-medium text-muted-foreground">Model</label>
              {provider === "openrouter" ? (
                <div ref={orRef} className="relative">
                  <div className="relative">
                    <Input
                      value={orOpen ? orQuery : model}
                      onChange={(e) => {
                        setOrQuery(e.target.value);
                        setModel(e.target.value);
                        if (!orOpen) setOrOpen(true);
                      }}
                      onFocus={() => {
                        setOrOpen(true);
                        setOrQuery(model);
                      }}
                      onKeyDown={(e) => {
                        if (!orOpen) return;
                        if (e.key === "ArrowDown") {
                          e.preventDefault();
                          setSelectedIndex((prev) => {
                            const next = prev < filteredOrModels.length - 1 ? prev + 1 : prev;
                            document.getElementById(`or-model-${next}`)?.scrollIntoView({ block: "nearest" });
                            return next;
                          });
                        } else if (e.key === "ArrowUp") {
                          e.preventDefault();
                          setSelectedIndex((prev) => {
                            const next = prev > 0 ? prev - 1 : 0;
                            document.getElementById(`or-model-${next}`)?.scrollIntoView({ block: "nearest" });
                            return next;
                          });
                        } else if (e.key === "Enter") {
                          e.preventDefault();
                          const selected = filteredOrModels[selectedIndex];
                          if (selected) {
                            setModel(selected.id);
                            setOrQuery(selected.id);
                            setOrOpen(false);
                          }
                        } else if (e.key === "Escape") {
                          setOrOpen(false);
                        }
                      }}
                      placeholder={orLoading ? "Loading models..." : "Search OpenRouter models..."}
                      className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm"
                    />
                    {orLoading && (
                      <Spinner className="size-3.5 text-muted-foreground absolute right-3 top-1/2 -translate-y-1/2" />
                    )}
                  </div>
                  {orOpen && filteredOrModels.length > 0 && (
                    <div className="absolute z-50 left-0 right-0 top-full mt-1.5 p-1 max-h-[240px] overflow-y-auto rounded-xl border border-border bg-popover text-popover-foreground shadow-md outline-none custom-scrollbar">
                      {filteredOrModels.map((m, i) => (
                        <button
                          id={`or-model-${i}`}
                          key={m.id}
                          onClick={() => {
                            setModel(m.id);
                            setOrQuery(m.id);
                            setOrOpen(false);
                          }}
                          onMouseEnter={() => setSelectedIndex(i)}
                          className={`w-full flex flex-col items-start rounded-sm px-2 py-1.5 text-left transition-colors hover:bg-accent hover:text-accent-foreground ${
                            selectedIndex === i || (!orQuery && model === m.id)
                              ? "bg-accent text-accent-foreground"
                              : ""
                          }`}
                        >
                          <span className="text-xs font-mono font-medium">{m.id}</span>
                          {m.name !== m.id && (
                            <span className="text-[10px] text-muted-foreground truncate w-full mt-0.5">{m.name}</span>
                          )}
                        </button>
                      ))}
                    </div>
                  )}
                  {orOpen && !orLoading && orQuery && filteredOrModels.length === 0 && (
                    <div className="absolute z-50 left-0 right-0 mt-0 rounded-b-xl border border-t-0 border-border bg-popover shadow-lg px-3 py-3 text-xs text-muted-foreground">
                      No models matching &ldquo;{orQuery}&rdquo;
                    </div>
                  )}
                </div>
              ) : help.models.length > 0 ? (
                <div className="space-y-1.5">
                  <Input
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    placeholder="Model name"
                    className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm"
                  />
                  <div className="flex flex-wrap gap-1">
                    {help.models.slice(0, 4).map((m) => (
                      <button
                        key={m}
                        onClick={() => setModel(m)}
                        className={`text-[10px] px-2 py-0.5 rounded-full border transition-colors ${
                          model === m
                            ? "bg-primary/10 border-primary/20 text-primary font-semibold"
                            : "border-border text-muted-foreground hover:text-foreground hover:border-muted-foreground/30"
                        }`}
                      >
                        {m}
                      </button>
                    ))}
                  </div>
                </div>
              ) : (
                <Input
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                  placeholder="Model name (e.g. gpt-4o)"
                  className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm"
                />
              )}
            </div>

            {/* API Key */}
            {needsApiKey && (
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <label className="text-xs font-medium text-muted-foreground">
                    API Key {llm?.has_api_key && !apiKey && <span className="text-success">(saved)</span>}
                  </label>
                  {help.docs && (
                    <a
                      href={help.docs}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-[10px] text-primary hover:underline flex items-center gap-0.5"
                    >
                      Get API key <HugeiconsIcon icon={LinkSquare01Icon} className="size-2" />
                    </a>
                  )}
                </div>
                <div className="relative">
                  <Input
                    type={showApiKey ? "text" : "password"}
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                    placeholder={llm?.has_api_key ? "••••••••  (unchanged)" : "sk-or-v1-..."}
                    className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm pr-10"
                  />
                  <button
                    onClick={() => setShowApiKey(!showApiKey)}
                    className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground transition-colors"
                  >
                    {showApiKey ? <HugeiconsIcon icon={ViewOffIcon} className="size-3.5" /> : <HugeiconsIcon icon={ViewIcon} className="size-3.5" />}
                  </button>
                </div>
                <p className="text-[10px] text-muted-foreground flex items-center gap-1">
                  <HugeiconsIcon icon={SquareLock01Icon} className="size-2.5" /> Encrypted at rest · never sent to AI
                </p>
              </div>
            )}

            {/* Base URL */}
            {needsBaseUrl && (
              <div className="space-y-2">
                <label className="text-xs font-medium text-muted-foreground">Endpoint URL</label>
                <Input
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                  placeholder={help.placeholder}
                  className="h-10 rounded-lg bg-secondary/50 border-border font-mono text-sm"
                />
              </div>
            )}

            {/* Temperature */}
            <div className="space-y-2">
              <label className="text-xs font-medium text-muted-foreground">
                Temperature <span className="text-foreground font-mono ml-1">{temperature}</span>
              </label>
              <input
                type="range"
                min="0"
                max="1"
                step="0.1"
                value={temperature}
                onChange={(e) => setTemperature(parseFloat(e.target.value))}
                className="w-full accent-primary h-1.5"
              />
              <div className="flex justify-between text-[10px] text-muted-foreground">
                <span>Precise</span>
                <span>Creative</span>
              </div>
            </div>
          </div>

          {/* Quick Start Hint */}
          {quickStartHint && provider === "openrouter" && !llm?.has_api_key && !apiKey && (
            <div className="flex items-start gap-3 p-3.5 rounded-xl bg-primary/5 border border-primary/10">
              <HugeiconsIcon icon={ArrowRight01Icon} className="size-4 text-primary mt-0.5 shrink-0" />
              <div className="space-y-1">
                <p className="text-xs font-medium text-foreground">Quick Start with OpenRouter</p>
                <p className="text-[10px] text-muted-foreground leading-relaxed">
                  1. Go to{" "}
                  <a href="https://openrouter.ai/keys" target="_blank" rel="noopener noreferrer" className="text-primary hover:underline">
                    openrouter.ai/keys
                  </a>{" "}
                  and create an API key<br />
                  2. Paste it in the API Key field above<br />
                  3. Click Save — you can start building agents immediately
                </p>
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

// ── Provider category badge ─────────────────────────────────────

function ProviderCategoryBadge({ category }: { category: "self-hosted" | "ws" | "http" }) {
  if (category === "self-hosted") {
    return (
      <span className="inline-flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground bg-foreground/5 px-1.5 py-0.5 rounded-full">
        <HugeiconsIcon icon={ServerStack01Icon} className="size-3" />Self-hosted
      </span>
    );
  }
  if (category === "ws") {
    return (
      <span className="inline-flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground bg-foreground/5 px-1.5 py-0.5 rounded-full">
        <HugeiconsIcon icon={LiveStreaming02Icon} className="size-3" />WebSocket
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground bg-foreground/5 px-1.5 py-0.5 rounded-full">
      <HugeiconsIcon icon={CloudIcon} className="size-3" />HTTP
    </span>
  );
}

// STT providers that use WebSocket despite not having a "-ws" suffix.
// (In TTS, 'cartesia' and 'deepgram' are HTTP — only 'cartesia-ws' / 'deepgram-ws' are WS.)
const STT_WS_PROVIDERS = new Set([
  "deepgram",        // STT — Deepgram nova-3 WS
  "cartesia",        // STT — Cartesia Ink Whisper WS
  "openai-realtime", // STT — GPT-4o Realtime WS
]);

function getProviderCategory(
  value: string,
  type: "stt" | "tts",
): "self-hosted" | "ws" | "http" {
  if (value === "faster-whisper" || value === "fish-speech") return "self-hosted";
  if (value.endsWith("-ws")) return "ws";
  if (type === "stt" && STT_WS_PROVIDERS.has(value)) return "ws";
  return "http";
}

// ── Voice Provider Card (full-featured for STT/TTS) ─────────────

// Language badge — uses labels fetched from GET /api/agents/languages (Rust source of truth).
// The languageMap prop is code→label, e.g. { en: "English", pt: "Portuguese" }.
function LangBadge({ code, languageMap }: { code: string; languageMap: Record<string, string> }) {
  return (
    <span className="inline-flex items-center px-1 py-0.5 rounded text-[10px] font-semibold bg-secondary text-muted-foreground border border-border/60 leading-none">
      {languageMap[code] ? `${code.toUpperCase()} · ${languageMap[code]}` : code.toUpperCase()}
    </span>
  );
}

function VoiceProviderCard({
  icon: Icon,
  title,
  subtitle,
  settings,
  providers,
  provider,
  onProviderChange,
  baseUrl,
  onBaseUrlChange,
  config,
  onConfigChange,
  loading,
  type,
}: {
  icon: IconSvgElement;
  title: string;
  subtitle: string;
  settings: VoiceProviderSettings | null;
  providers: VoiceProviderOption[];
  provider: string;
  onProviderChange: (value: string, option?: VoiceProviderOption) => void;
  baseUrl: string;
  onBaseUrlChange: (url: string) => void;
  config: Record<string, string>;
  onConfigChange: (config: Record<string, string>) => void;
  loading: boolean;
  type: "stt" | "tts";
}) {
  const [showSecrets, setShowSecrets] = useState<Record<string, boolean>>({});
  const [modelCatalog, setModelCatalog] = useState<SttModelSpec[]>([]);
  const [languageMap, setLanguageMap] = useState<Record<string, string>>({});

  const selectedProvider = providers.find((p) => p.value === provider);
  const isConfigured =
    settings?.all_credentials?.[provider] ||
    (settings?.provider === provider && settings?.has_api_key) ||
    config.api_key ||
    provider === "faster-whisper" ||
    provider === "fish-speech";
  const category = getProviderCategory(provider, type);

  // Exclude voice_id (lives on Agent Config page) and model (handled by Select below)
  const regularFields: VoiceProviderField[] = (selectedProvider?.fields || []).filter(
    (f) => !f.is_secret && f.key !== "voice_id" && f.key !== "model",
  );
  const secretFields: VoiceProviderField[] = (selectedProvider?.fields || []).filter((f) => f.is_secret);
  const isSelfHosted = category === "self-hosted";

  // Fetch language label map from Rust SUPPORTED_LANGUAGES via GET /api/agents/languages
  useEffect(() => {
    api.agents.getLanguages()
      .then((langs) => {
        const map: Record<string, string> = {};
        for (const l of langs) map[l.code] = l.label;
        setLanguageMap(map);
      })
      .catch(() => {});
  }, []);

  // Fetch model catalog from the Rust TTS_MODEL_CATALOG when provider changes
  useEffect(() => {
    let active = true;
    const fetchCatalog = async () => {
      if (!provider || provider === "faster-whisper" || provider === "fish-speech") {
        if (active) setModelCatalog([]);
        return;
      }
      try {
        const fetchFn = type === "tts"
          ? () => api.settings.getTtsCatalog(provider)
          : () => api.settings.getSttCatalog(provider);
        const res = await fetchFn();
        if (active) setModelCatalog(res);
      } catch {
        if (active) setModelCatalog([]);
      }
    };
    fetchCatalog();
    return () => { active = false; };
  }, [provider, type]);

  const selectedModelSpec = modelCatalog.find((m) => m.model_id === config.model);

  return (
    <section className="flat-card p-6">
      <div className="flex items-center gap-3 mb-6">
        <div className="size-9 rounded-xl bg-secondary flex items-center justify-center">
          <HugeiconsIcon icon={Icon} className="size-4 text-foreground" />
        </div>
        <div className="flex-1">
          <h3 className="text-sm font-semibold text-foreground">{title}</h3>
          <p className="text-xs text-muted-foreground mt-0.5">{subtitle}</p>
        </div>
        {settings && (
          <Badge
            className={`${
              isConfigured
                ? "bg-primary/10 text-primary"
                : "bg-destructive/10 text-destructive"
            } border-none shadow-none text-[10px] font-medium rounded-full`}
          >
            {isConfigured ? "Configured" : "Needs API Key"}
          </Badge>
        )}
      </div>

      {loading ? (
        <div className="flex justify-center py-10">
          <Spinner className="size-5 text-muted-foreground" />
        </div>
      ) : (
        <div className="space-y-5">
          {/* Provider grid */}
          <div className="space-y-2">
            <label className="text-xs font-medium text-muted-foreground">Provider</label>
            <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 gap-2">
              {providers.map((p) => {
                const cat = getProviderCategory(p.value, type);
                const hasSavedCreds = settings?.all_credentials?.[p.value] ?? false;
                return (
                  <button
                    key={p.value}
                    onClick={() => onProviderChange(p.value, p)}
                    className={`relative flex flex-col items-start gap-1.5 p-3 rounded-xl border transition-all text-left ${
                      provider === p.value
                        ? "border-primary bg-primary/5 ring-1 ring-primary/20"
                        : "border-border hover:border-muted-foreground/30 bg-background"
                    }`}
                  >
                    <div className="flex items-center justify-between w-full">
                      <span className="text-xs font-semibold text-foreground flex items-center gap-2">
                        {hasSavedCreds && (
                          <TooltipProvider>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <div className="size-1.5 rounded-full bg-emerald-500 shadow-[0_0_5px_rgba(16,185,129,0.5)] shrink-0" />
                              </TooltipTrigger>
                              <TooltipContent side="top">
                                <p className="text-[10px] font-medium">Saved credentials</p>
                              </TooltipContent>
                            </Tooltip>
                          </TooltipProvider>
                        )}
                        {p.label}
                      </span>
                      <div className="flex items-center gap-1">
                        <ProviderCategoryBadge category={cat} />
                      </div>
                    </div>
                    <span className="text-[10px] text-muted-foreground leading-tight">{p.description}</span>
                  </button>
                );
              })}
            </div>
          </div>

          {/* Config fields */}
          <div className="space-y-3">
            {/* Base URL — only for self-hosted */}
            {isSelfHosted && (
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground">Base URL</label>
                <Input
                  value={baseUrl}
                  onChange={(e) => onBaseUrlChange(e.target.value)}
                  placeholder={selectedProvider?.default_base_url || "http://localhost:8000"}
                  className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                />
              </div>
            )}

            {/* Secret fields (API key) — always first */}
            {secretFields.length > 0 && (
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                {secretFields.map((field) => {
                  const isShown = showSecrets[field.key] ?? false;
                  const hasSaved =
                    (settings?.has_api_key || settings?.all_credentials?.[provider]) &&
                    !config[field.key];
                  return (
                    <div key={field.key} className="space-y-1.5">
                      <div className="flex items-center justify-between">
                        <label className="text-xs font-medium text-muted-foreground">
                          {field.label}
                          {hasSaved && <span className="text-success ml-1.5">(saved)</span>}
                        </label>
                        {selectedProvider?.docs_url && (
                          <a
                            href={selectedProvider.docs_url}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="text-[10px] text-primary hover:underline flex items-center gap-0.5"
                          >
                            Get key <HugeiconsIcon icon={LinkSquare01Icon} className="size-2" />
                          </a>
                        )}
                      </div>
                      <div className="relative">
                        <Input
                          type={isShown ? "text" : "password"}
                          value={config[field.key] || ""}
                          onChange={(e) => onConfigChange({ ...config, [field.key]: e.target.value })}
                          placeholder={hasSaved ? "••••••••  (unchanged)" : field.placeholder || "sk-..."}
                          className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs pr-9"
                        />
                        <button
                          onClick={() => setShowSecrets((prev) => ({ ...prev, [field.key]: !isShown }))}
                          className="absolute right-2.5 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground transition-colors"
                        >
                          {isShown ? <HugeiconsIcon icon={ViewOffIcon} className="size-3.5" /> : <HugeiconsIcon icon={ViewIcon} className="size-3.5" />}
                        </button>
                      </div>
                      <p className="text-[10px] text-muted-foreground flex items-center gap-1">
                        <HugeiconsIcon icon={SquareLock01Icon} className="size-2.5" /> Encrypted at rest
                      </p>
                    </div>
                  );
                })}
              </div>
            )}

            {/* Model — Select (catalog) or text input fallback — always visible for cloud providers */}
            {!isSelfHosted && (
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground">Default Model</label>
                {modelCatalog.length > 0 ? (
                  <>
                    <Select
                      value={config.model || undefined}
                      onValueChange={(val) => onConfigChange({ ...config, model: val })}
                    >
                      <SelectTrigger className="h-9 rounded-lg bg-secondary/50 border-border text-xs font-mono">
                        <SelectValue placeholder="Select a model…">
                          {selectedModelSpec?.label ?? config.model ?? null}
                        </SelectValue>
                      </SelectTrigger>
                      <SelectContent>
                        {modelCatalog.map((m) => (
                          <SelectItem key={m.model_id} value={m.model_id} className="text-xs">
                            {m.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    {/* Language badges — labels from Rust SUPPORTED_LANGUAGES via /api/agents/languages */}
                    {selectedModelSpec && selectedModelSpec.supported_languages.length > 0 && (
                      <div className="flex flex-wrap gap-1 pt-1">
                        <span className="text-[10px] text-muted-foreground self-center mr-0.5">Supports:</span>
                        {selectedModelSpec.supported_languages.map((lang) => (
                          <LangBadge key={lang} code={lang} languageMap={languageMap} />
                        ))}
                      </div>
                    )}
                  </>
                ) : (
                  <Input
                    value={config.model || ""}
                    onChange={(e) => onConfigChange({ ...config, model: e.target.value })}
                    placeholder={selectedProvider?.fields?.find((f) => f.key === "model")?.placeholder || "model-id"}
                    className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                  />
                )}
              </div>
            )}

            {/* Remaining regular fields (not model, not voice_id) */}
            {regularFields.length > 0 && (
              <div className={`grid gap-3 ${regularFields.length === 1 ? "grid-cols-1" : "grid-cols-2"}`}>
                {regularFields.map((field) => (
                  <div key={field.key} className="space-y-1.5">
                    <label className="text-xs font-medium text-muted-foreground">{field.label}</label>
                    <Input
                      value={config[field.key] || ""}
                      onChange={(e) => onConfigChange({ ...config, [field.key]: e.target.value })}
                      placeholder={field.placeholder}
                      className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                    />
                  </div>
                ))}
              </div>
            )}

            {/* WS latency hint */}
            {category === "ws" && (
              <div className="flex items-center gap-2 p-2.5 rounded-lg bg-success/5 border border-success/10 text-success text-[10px]">
                <HugeiconsIcon icon={LiveStreaming02Icon} className="size-3 shrink-0" />
                <span>WebSocket mode — persistent connection, lowest latency (~50–200 ms saved per turn)</span>
              </div>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

// ── Observability defaults — single source of truth on the frontend ──
const OBS_DEFAULTS: {
  queue_size: number;
  batch_size: number;
  flush_interval_ms: number;
  shutdown_flush_timeout_ms: number;
  drop_policy: string;
  db_categories: string[];
  db_event_types: string[];
} = {
  queue_size: 2048,
  batch_size: 128,
  flush_interval_ms: 1000,
  shutdown_flush_timeout_ms: 1500,
  drop_policy: "drop_oldest",
  db_categories: ["session", "metrics", "observability", "tool", "error"],
  db_event_types: [],
};

// ── Page ─────────────────────────────────────────────────────────

function SettingsPageContent() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const headerRef = useRef<HTMLDivElement | null>(null);

  const validTabIds = SETTINGS_TABS.map((t) => t.id);
  const tabParam = searchParams.get("tab") as SettingsSectionId | null;
  const activeTab: SettingsSectionId =
    tabParam && validTabIds.includes(tabParam) ? tabParam : "ai-models";

  const setActiveTab = useCallback(
    (tab: SettingsSectionId) => {
      const params = new URLSearchParams(searchParams.toString());
      params.set("tab", tab);
      router.replace(`?${params.toString()}`, { scroll: false });
    },
    [router, searchParams],
  );

  const [isHeaderVisible, setIsHeaderVisible] = useState(true);

  // Builder LLM state
  const [builderLlm, setBuilderLlm] = useState<LLMSettings | null>(null);
  const [builderProviders, setBuilderProviders] = useState<LLMProviderOption[]>([]);
  const [builderProvider, setBuilderProvider] = useState("ollama");
  const [builderModel, setBuilderModel] = useState("llama3.2");
  const [builderBaseUrl, setBuilderBaseUrl] = useState("http://localhost:11434");
  const [builderApiKey, setBuilderApiKey] = useState("");
  const [builderTemperature, setBuilderTemperature] = useState(0.7);
  const [builderShowApiKey, setBuilderShowApiKey] = useState(false);

  // Voice Agent LLM state (reuses same LLM providers but persisted separately)
  const [voiceLlm, setVoiceLlm] = useState<LLMSettings | null>(null);
  const [voiceProviders, setVoiceProviders] = useState<LLMProviderOption[]>([]);
  const [voiceProvider, setVoiceProvider] = useState("ollama");
  const [voiceModel, setVoiceModel] = useState("llama3.2");
  const [voiceBaseUrl, setVoiceBaseUrl] = useState("http://localhost:11434");
  const [voiceApiKey, setVoiceApiKey] = useState("");
  const [voiceTemperature, setVoiceTemperature] = useState(0.7);
  const [voiceShowApiKey, setVoiceShowApiKey] = useState(false);

  // STT state
  const [stt, setSTT] = useState<VoiceProviderSettings | null>(null);
  const [sttProviders, setSTTProviders] = useState<VoiceProviderOption[]>([]);
  const [sttProvider, setSTTProvider] = useState("faster-whisper");
  const [sttBaseUrl, setSTTBaseUrl] = useState("http://localhost:8100");
  const [sttConfig, setSTTConfig] = useState<Record<string, string>>({});

  // TTS state
  const [tts, setTTS] = useState<VoiceProviderSettings | null>(null);
  const [ttsProviders, setTTSProviders] = useState<VoiceProviderOption[]>([]);
  const [ttsProvider, setTTSProvider] = useState("kokoro");
  const [ttsBaseUrl, setTTSBaseUrl] = useState("http://localhost:8200");
  const [ttsConfig, setTTSConfig] = useState<Record<string, string>>({});

  // Telephony state
  const [voiceServerUrl, setVoiceServerUrl] = useState("");
  // Track the last-saved URL so we can detect changes
  const savedVoiceServerUrl = useRef("");

  // Observability state
  const [observability, setObservability] = useState<ObservabilitySettings | null>(null);
  const [dbEventsEnabled, setDbEventsEnabled] = useState(true);
  const [langfuseEnabled, setLangfuseEnabled] = useState(false);
  const [langfuseBaseUrl, setLangfuseBaseUrl] = useState("https://cloud.langfuse.com");
  const [langfusePublicKey, setLangfusePublicKey] = useState("");
  const [langfuseSecretKey, setLangfuseSecretKey] = useState("");
  const [langfuseTracePublic, setLangfuseTracePublic] = useState(false);

  const [queueSize, setQueueSize] = useState(OBS_DEFAULTS.queue_size);
  const [batchSize, setBatchSize] = useState(OBS_DEFAULTS.batch_size);
  const [flushIntervalMs, setFlushIntervalMs] = useState(OBS_DEFAULTS.flush_interval_ms);
  const [shutdownFlushTimeoutMs, setShutdownFlushTimeoutMs] = useState(OBS_DEFAULTS.shutdown_flush_timeout_ms);
  const [dropPolicy, setDropPolicy] = useState(OBS_DEFAULTS.drop_policy);
  const [showAdvancedDb, setShowAdvancedDb] = useState(false);

  const handleLangfuseEnvPaste = useCallback(
    (e: ClipboardEvent<HTMLInputElement>) => {
      const text = e.clipboardData.getData("text");
      if (!text) return;

      const parsed: Record<string, string> = {};
      const linePattern =
        /(?:^|\n)\s*(LANGFUSE_SECRET_KEY|LANGFUSE_PUBLIC_KEY|LANGFUSE_BASE_URL)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\n#]+))/g;
      let match: RegExpExecArray | null = null;
      while ((match = linePattern.exec(text)) !== null) {
        const key = match[1];
        const value = (match[2] ?? match[3] ?? match[4] ?? "").trim();
        if (key && value) parsed[key] = value;
      }

      const hasAny =
        Boolean(parsed.LANGFUSE_BASE_URL) ||
        Boolean(parsed.LANGFUSE_PUBLIC_KEY) ||
        Boolean(parsed.LANGFUSE_SECRET_KEY);
      if (!hasAny) return;

      e.preventDefault();
      if (parsed.LANGFUSE_BASE_URL) setLangfuseBaseUrl(parsed.LANGFUSE_BASE_URL);
      if (parsed.LANGFUSE_PUBLIC_KEY) setLangfusePublicKey(parsed.LANGFUSE_PUBLIC_KEY);
      if (parsed.LANGFUSE_SECRET_KEY) setLangfuseSecretKey(parsed.LANGFUSE_SECRET_KEY);
    },
    []
  );

  // General state
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const langfuseMissingRequiredKeys =
    langfuseEnabled &&
    (
      (!langfusePublicKey && !(observability?.langfuse_has_public_key ?? false)) ||
      (!langfuseSecretKey && !(observability?.langfuse_has_secret_key ?? false))
    );

  const loadSettings = useCallback(async () => {
    try {
      setLoading(true);
      const [llmRes, voiceLlmRes, sttRes, ttsRes, telephonyRes, observabilityRes] = await Promise.all([
        api.settings.getLLM(),
        api.settings.getVoiceLLM(),
        api.settings.getSTT(),
        api.settings.getTTS(),
        api.settings.getTelephony(),
        api.settings.getObservability(),
      ]);

      // Builder LLM
      setBuilderLlm(llmRes);
      setBuilderProviders(llmRes.supported_providers);
      setBuilderProvider(llmRes.provider);
      setBuilderModel(llmRes.model);
      setBuilderBaseUrl(llmRes.base_url);
      setBuilderTemperature(llmRes.temperature);
      setBuilderApiKey("");

      // Voice Agent LLM
      setVoiceLlm(voiceLlmRes);
      setVoiceProviders(voiceLlmRes.supported_providers);
      setVoiceProvider(voiceLlmRes.provider);
      setVoiceModel(voiceLlmRes.model);
      setVoiceBaseUrl(voiceLlmRes.base_url);
      setVoiceTemperature(voiceLlmRes.temperature);
      setVoiceApiKey("");

      // STT
      setSTT(sttRes);
      setSTTProviders(sttRes.supported_providers);
      setSTTProvider(sttRes.provider);
      setSTTBaseUrl(sttRes.base_url);
      setSTTConfig(sttRes.config);

      // TTS
      setTTS(ttsRes);
      setTTSProviders(ttsRes.supported_providers);
      setTTSProvider(ttsRes.provider);
      setTTSBaseUrl(ttsRes.base_url);
      setTTSConfig(ttsRes.config);

      // Telephony
      const initialUrl = telephonyRes.voice_server_url || DEFAULT_VOICE_SERVER_URL;
      setVoiceServerUrl(initialUrl);
      savedVoiceServerUrl.current = initialUrl;

      // Observability
      setObservability(observabilityRes);
      setDbEventsEnabled(observabilityRes.db_events_enabled);
      setLangfuseEnabled(observabilityRes.langfuse_enabled);
      setLangfuseBaseUrl(observabilityRes.langfuse_base_url || "https://cloud.langfuse.com");
      setLangfusePublicKey("");
      setLangfuseSecretKey("");
      setLangfuseTracePublic(observabilityRes.langfuse_trace_public ?? false);
      setQueueSize(observabilityRes.queue_size ?? OBS_DEFAULTS.queue_size);
      setBatchSize(observabilityRes.batch_size ?? OBS_DEFAULTS.batch_size);
      setFlushIntervalMs(observabilityRes.flush_interval_ms ?? OBS_DEFAULTS.flush_interval_ms);
      setShutdownFlushTimeoutMs(observabilityRes.shutdown_flush_timeout_ms ?? OBS_DEFAULTS.shutdown_flush_timeout_ms);
      setDropPolicy(observabilityRes.drop_policy ?? OBS_DEFAULTS.drop_policy);
    } catch {
      setError("Could not load settings. Is the backend running?");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { loadSettings(); }, [loadSettings]);

  useEffect(() => {
    const headerNode = headerRef.current;
    if (!headerNode) return;

    const observer = new IntersectionObserver(
      ([entry]) => setIsHeaderVisible(entry.isIntersecting),
      { threshold: 0.05 }
    );
    observer.observe(headerNode);

    return () => observer.disconnect();
  }, []);

  const saveSettings = async () => {
    if (!voiceServerUrl) {
      setError("Voice Server URL is required.");
      return;
    }
    if (!isValidVoiceServerUrl(voiceServerUrl)) {
      setError("Voice Server URL must start with http:// or https://");
      return;
    }
    if (langfuseEnabled) {
      if (!langfuseBaseUrl || !isValidVoiceServerUrl(langfuseBaseUrl)) {
        setError("Langfuse URL must start with http:// or https://");
        return;
      }
      const hasSavedPublic = observability?.langfuse_has_public_key ?? false;
      const hasSavedSecret = observability?.langfuse_has_secret_key ?? false;
      if (!langfusePublicKey && !hasSavedPublic) {
        setError("Langfuse Public Key is required when Langfuse is enabled.");
        return;
      }
      if (!langfuseSecretKey && !hasSavedSecret) {
        setError("Langfuse Secret Key is required when Langfuse is enabled.");
        return;
      }
    }

    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      // Builder LLM
      const builderUpdate: LLMSettingsUpdate = {
        provider: builderProvider,
        model: builderModel,
        base_url: builderBaseUrl,
        temperature: builderTemperature,
      };
      if (builderApiKey) builderUpdate.api_key = builderApiKey;

      // Voice Agent LLM
      const voiceUpdate: LLMSettingsUpdate = {
        provider: voiceProvider,
        model: voiceModel,
        base_url: voiceBaseUrl,
        temperature: voiceTemperature,
      };
      if (voiceApiKey) voiceUpdate.api_key = voiceApiKey;

      // STT — pass api_key in config dict; backend extracts + encrypts it
      const sttUpdate: VoiceProviderUpdate = {
        provider: sttProvider,
        base_url: sttBaseUrl,
        config: { ...sttConfig },
      };

      // TTS
      const ttsUpdate: VoiceProviderUpdate = {
        provider: ttsProvider,
        base_url: ttsBaseUrl,
        config: { ...ttsConfig },
      };

      // Telephony — only voice_server_url
      const telephonyUpdate: TelephonySettingsUpdate = {
        voice_server_url: voiceServerUrl,
      };
      const observabilityUpdate: ObservabilitySettingsUpdate = {
        db_events_enabled: dbEventsEnabled,
        langfuse_enabled: langfuseEnabled,
        langfuse_base_url: langfuseBaseUrl,
        langfuse_trace_public: langfuseTracePublic,
        queue_size: queueSize,
        batch_size: batchSize,
        flush_interval_ms: flushIntervalMs,
        shutdown_flush_timeout_ms: shutdownFlushTimeoutMs,
        drop_policy: dropPolicy,
        db_categories: observability?.db_categories ?? OBS_DEFAULTS.db_categories,
        db_event_types: observability?.db_event_types ?? OBS_DEFAULTS.db_event_types,
      };
      if (langfusePublicKey) observabilityUpdate.langfuse_public_key = langfusePublicKey;
      if (langfuseSecretKey) observabilityUpdate.langfuse_secret_key = langfuseSecretKey;

      const [llmResult, voiceLlmResult, sttResult, ttsResult, telephonyResult, observabilityResult] = await Promise.all([
        api.settings.updateLLM(builderUpdate),
        api.settings.updateVoiceLLM(voiceUpdate),
        api.settings.updateSTT(sttUpdate),
        api.settings.updateTTS(ttsUpdate),
        api.settings.updateTelephony(telephonyUpdate),
        api.settings.updateObservability(observabilityUpdate),
      ]);

      setBuilderLlm(llmResult);
      setBuilderApiKey("");
      setVoiceLlm(voiceLlmResult);
      setVoiceApiKey("");
      setSTT(sttResult);
      // Clear sensitive fields so password inputs show "(saved)"
      setSTTConfig((prev) => {
        const next = { ...prev };
        delete next.api_key;
        return next;
      });
      setTTS(ttsResult);
      setTTSConfig((prev) => {
        const next = { ...prev };
        delete next.api_key;
        return next;
      });
      const newUrl = telephonyResult.voice_server_url || DEFAULT_VOICE_SERVER_URL;
      if (newUrl !== savedVoiceServerUrl.current) {
        toast.warning("Restart voice-server", {
          description:
            "The Voice Server URL was changed. Restart voice-server for it to take effect on incoming calls.",
          duration: 8000,
        });
      }
      savedVoiceServerUrl.current = newUrl;
      setVoiceServerUrl(newUrl);
      const prevObs = observability; // snapshot before setObservability overwrites it
      setObservability(observabilityResult);
      setDbEventsEnabled(observabilityResult.db_events_enabled);
      setLangfuseEnabled(observabilityResult.langfuse_enabled);
      setLangfuseBaseUrl(observabilityResult.langfuse_base_url);
      setLangfusePublicKey("");
      setLangfuseSecretKey("");
      setLangfuseTracePublic(observabilityResult.langfuse_trace_public);
      setQueueSize(observabilityResult.queue_size ?? OBS_DEFAULTS.queue_size);
      setBatchSize(observabilityResult.batch_size ?? OBS_DEFAULTS.batch_size);
      setFlushIntervalMs(observabilityResult.flush_interval_ms ?? OBS_DEFAULTS.flush_interval_ms);
      setShutdownFlushTimeoutMs(observabilityResult.shutdown_flush_timeout_ms ?? OBS_DEFAULTS.shutdown_flush_timeout_ms);
      setDropPolicy(observabilityResult.drop_policy ?? OBS_DEFAULTS.drop_policy);

      // voice-server loads observability settings once at startup — any change
      // requires a restart to take effect on active and future calls.
      const obsChanged = !prevObs || (
        prevObs.db_events_enabled !== observabilityResult.db_events_enabled ||
        prevObs.langfuse_enabled !== observabilityResult.langfuse_enabled ||
        prevObs.langfuse_base_url !== observabilityResult.langfuse_base_url ||
        prevObs.langfuse_trace_public !== observabilityResult.langfuse_trace_public ||
        prevObs.queue_size !== observabilityResult.queue_size ||
        prevObs.batch_size !== observabilityResult.batch_size ||
        prevObs.flush_interval_ms !== observabilityResult.flush_interval_ms ||
        prevObs.shutdown_flush_timeout_ms !== observabilityResult.shutdown_flush_timeout_ms ||
        prevObs.drop_policy !== observabilityResult.drop_policy ||
        // treat key presence changes as a change (actual key values are redacted)
        prevObs.langfuse_has_public_key !== observabilityResult.langfuse_has_public_key ||
        prevObs.langfuse_has_secret_key !== observabilityResult.langfuse_has_secret_key
      );
      if (obsChanged) {
        toast.warning("Restart voice-server", {
          description:
            "Observability settings were changed. Restart voice-server for them to take effect on active and future calls.",
          duration: 8000,
        });
      }
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to save settings");
      // Rollback optimistic observability state to last known-good values
      if (observability) {
        setLangfuseTracePublic(observability.langfuse_trace_public);
        setQueueSize(observability.queue_size ?? OBS_DEFAULTS.queue_size);
        setBatchSize(observability.batch_size ?? OBS_DEFAULTS.batch_size);
        setFlushIntervalMs(observability.flush_interval_ms ?? OBS_DEFAULTS.flush_interval_ms);
        setShutdownFlushTimeoutMs(observability.shutdown_flush_timeout_ms ?? OBS_DEFAULTS.shutdown_flush_timeout_ms);
        setDropPolicy(observability.drop_policy ?? OBS_DEFAULTS.drop_policy);
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-6 pb-28">
      {/* Header */}
      <div ref={headerRef} className="flex items-center justify-between">
        <PageHeader
          icon={Settings01Icon}
          title="Settings"
          description="Configure your workspace, LLM providers, and voice pipeline"
        />
        <div className="flex items-center gap-2">
          {saved && (
            <div className="flex items-center gap-1.5 text-success text-xs font-medium animate-in fade-in duration-300">
              <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-3.5" /> Saved
            </div>
          )}
          <Button
            onClick={saveSettings}
            disabled={saving || loading}
            className="h-8 px-5 text-xs font-medium gap-1.5"
          >
            {saving ? (
              <><Spinner className="size-3.5" /> Saving...</>
            ) : (
              <><HugeiconsIcon icon={SettingDone02Icon} className="size-3.5" /> Save Changes</>
            )}
          </Button>
        </div>
      </div>

      {error && (
        <div className="flex items-center gap-2.5 p-3 rounded-xl bg-destructive/5 border border-destructive/10 text-destructive text-sm">
          <HugeiconsIcon icon={AlertCircleIcon} className="size-4 shrink-0" />
          {error}
        </div>
      )}

      <div className="sticky top-0 z-30 -mx-2 px-2 bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/90 border-b border-border/70">
        <div className="flex items-end gap-7">
          {SETTINGS_TABS.map((tab) => {
            const isActive = activeTab === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                onClick={() => setActiveTab(tab.id)}
                className={`h-11 border-b-2 -mb-px text-sm transition-colors ${
                  isActive
                    ? "font-semibold text-foreground border-primary"
                    : "font-normal text-muted-foreground border-transparent hover:text-foreground/90"
                }`}
              >
                {tab.label}
              </button>
            );
          })}
        </div>
      </div>

      {/* ═══ SECTION 1: AI Models ═══ */}
      {activeTab === "ai-models" && (
      <div className="space-y-5">
        <div className="flex items-center gap-3">
          <div className="size-10 rounded-lg bg-primary/5 flex items-center justify-center">
            <HugeiconsIcon icon={AiCloudIcon} className="size-5 text-primary" />
          </div>
          <div>
            <h3 className="text-sm font-semibold text-foreground">AI Models</h3>
            <p className="text-xs text-muted-foreground mt-0.5">Configure the LLMs that power your builder and voice agents</p>
          </div>
        </div>

        <div className="space-y-4">
          <LLMCard
            title="Builder LLM"
            subtitle="The AI model that powers the agent builder chat"
            icon={AiProgrammingIcon}
            llm={builderLlm}
            providers={builderProviders}
            provider={builderProvider}
            setProvider={(val) => {
              setBuilderProvider(val);
              const saved = builderLlm?.all_configs?.[val];
              if (saved) {
                if (saved.model) setBuilderModel(saved.model);
                if (saved.base_url) setBuilderBaseUrl(saved.base_url);
                if (saved.temperature) setBuilderTemperature(parseFloat(saved.temperature));
              }
            }}
            model={builderModel}
            setModel={setBuilderModel}
            baseUrl={builderBaseUrl}
            setBaseUrl={setBuilderBaseUrl}
            apiKey={builderApiKey}
            setApiKey={setBuilderApiKey}
            temperature={builderTemperature}
            setTemperature={setBuilderTemperature}
            showApiKey={builderShowApiKey}
            setShowApiKey={setBuilderShowApiKey}
            loading={loading}
            quickStartHint
          />

          <LLMCard
            title="Voice Agent LLM"
            subtitle="The AI model used during live voice calls — optimise for speed"
            icon={AiMicIcon}
            llm={voiceLlm}
            providers={voiceProviders}
            provider={voiceProvider}
            setProvider={(val) => {
              setVoiceProvider(val);
              const saved = voiceLlm?.all_configs?.[val];
              if (saved) {
                if (saved.model) setVoiceModel(saved.model);
                if (saved.base_url) setVoiceBaseUrl(saved.base_url);
                if (saved.temperature) setVoiceTemperature(parseFloat(saved.temperature));
              }
            }}
            model={voiceModel}
            setModel={setVoiceModel}
            baseUrl={voiceBaseUrl}
            setBaseUrl={setVoiceBaseUrl}
            apiKey={voiceApiKey}
            setApiKey={setVoiceApiKey}
            temperature={voiceTemperature}
            setTemperature={setVoiceTemperature}
            showApiKey={voiceShowApiKey}
            setShowApiKey={setVoiceShowApiKey}
            loading={loading}
          />
        </div>
      </div>
      )}

      {/* ═══ SECTION 2: Voice Infrastructure ═══ */}
      {activeTab === "voice-infrastructure" && (
      <div className="space-y-5">
        <div className="flex items-center gap-3">
          <div className="size-10 rounded-lg bg-primary/5 flex items-center justify-center">
            <HugeiconsIcon icon={AiVoiceIcon} className="size-5 text-primary" />
          </div>
          <div>
            <h3 className="text-sm font-semibold text-foreground">Voice Infrastructure</h3>
            <p className="text-xs text-muted-foreground mt-0.5">Speech services and voice infrastructure for live calls</p>
          </div>
        </div>

        <div className="space-y-4">
          {/* STT Card */}
          <VoiceProviderCard
            icon={Mic01Icon}
            title="Transcription (STT)"
            subtitle="Speech-to-text — converts caller audio into text for the AI"
            settings={stt}
            providers={sttProviders}
            provider={sttProvider}
            onProviderChange={(val, opt) => {
              setSTTProvider(val);
              const savedConfig = stt?.all_configs?.[val];
              if (savedConfig) {
                const { base_url, ...rest } = savedConfig;
                if (base_url) setSTTBaseUrl(base_url);
                setSTTConfig(rest);
                return;
              }
              if (opt?.default_base_url) setSTTBaseUrl(opt.default_base_url);
              const defaults: Record<string, string> = {};
              for (const f of opt?.fields || []) {
                if (!f.is_secret) defaults[f.key] = "";
              }
              setSTTConfig(defaults);
            }}
            baseUrl={sttBaseUrl}
            onBaseUrlChange={setSTTBaseUrl}
            config={sttConfig}
            onConfigChange={setSTTConfig}
            loading={loading}
            type="stt"
          />

          {/* TTS Card */}
          <VoiceProviderCard
            icon={VolumeHighIcon}
            title="Synthesis (TTS)"
            subtitle="Text-to-speech — converts AI responses into natural-sounding audio"
            settings={tts}
            providers={ttsProviders}
            provider={ttsProvider}
            onProviderChange={(val, opt) => {
              setTTSProvider(val);
              const savedConfig = tts?.all_configs?.[val];
              if (savedConfig) {
                const { base_url, ...rest } = savedConfig;
                if (base_url) setTTSBaseUrl(base_url);
                setTTSConfig(rest);
                return;
              }
              if (opt?.default_base_url) setTTSBaseUrl(opt.default_base_url);
              const defaults: Record<string, string> = {};
              for (const f of opt?.fields || []) {
                if (!f.is_secret) defaults[f.key] = "";
              }
              setTTSConfig(defaults);
            }}
            baseUrl={ttsBaseUrl}
            onBaseUrlChange={setTTSBaseUrl}
            config={ttsConfig}
            onConfigChange={setTTSConfig}
            loading={loading}
            type="tts"
          />

          {/* Voice Server Card */}
          <section className="flat-card p-6">
            <div className="flex items-center gap-3 mb-5">
              <div className="size-9 rounded-xl bg-secondary flex items-center justify-center">
                <HugeiconsIcon icon={Call02Icon} className="size-4 text-foreground" />
              </div>
              <div className="flex-1">
                <h3 className="text-sm font-semibold text-foreground">Voice Server</h3>
                <p className="text-xs text-muted-foreground mt-0.5">Voice server URL for webhook configuration</p>
              </div>
            </div>

            {/* Voice Server URL */}
            <div className="space-y-2">
              <label className="text-xs text-muted-foreground flex items-center gap-1.5">
                <HugeiconsIcon icon={FlashIcon} className="size-3" /> Voice Server URL
              </label>
              <Input
                value={voiceServerUrl}
                onChange={(e) => setVoiceServerUrl(e.target.value)}
                placeholder={DEFAULT_VOICE_SERVER_URL}
                className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
              />
              <p className="text-[10px] text-muted-foreground/70 leading-relaxed">
                Used by the browser for voice test calls, and by Twilio/Telnyx for webhook URLs.<br />
                Local dev default: <span className="font-mono text-foreground/70">{DEFAULT_VOICE_SERVER_URL}</span>.<br />
                For Twilio/Telnyx webhooks, this must be a public HTTPS URL — use ngrok or deploy voice-server publicly.
              </p>
            </div>

            <div className="flex items-center gap-2 p-3 rounded-xl bg-orange-500/5 border border-orange-500/10 text-orange-500/80 text-[10px] leading-relaxed mt-6">
              <HugeiconsIcon icon={AlertCircleIcon} className="size-3.5 shrink-0" />
              <span>
                Provider credentials (Twilio/Telnyx) are now entered when importing numbers and stored per-number.
                Manage them in <a href="/dashboard/phone-numbers" className="font-medium underline">Phone Numbers</a>.
              </span>
            </div>
          </section>
        </div>
      </div>
      )}

      {/* ═══ SECTION 3: Observability ═══ */}
      {activeTab === "observability" && (
      <div className="space-y-5">
        <div className="flex items-center gap-3">
          <div className="size-10 rounded-lg bg-primary/5 flex items-center justify-center">
            <HugeiconsIcon icon={Analytics01Icon} className="size-5 text-primary" />
          </div>
          <div>
            <h3 className="text-sm font-semibold text-foreground">Observability</h3>
            <p className="text-xs text-muted-foreground mt-0.5">
              Control internal call events and Langfuse integration
            </p>
          </div>
        </div>

        <section className="flat-card p-6 space-y-5">
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm font-medium text-foreground">Internal DB Call Events</p>
              <p className="text-[10px] text-muted-foreground mt-0.5">
                Store call observability events in database for Log tab rendering
              </p>
            </div>
            <div className="flex items-center gap-2 text-xs text-foreground">
              <span>Enabled</span>
              <Switch
                checked={dbEventsEnabled}
                onCheckedChange={setDbEventsEnabled}
                aria-label="Enable DB call events"
              />
            </div>
          </div>

          <div className="pt-2">
            <button
              onClick={() => setShowAdvancedDb(!showAdvancedDb)}
              className="flex items-center gap-1.5 text-[10px] font-medium text-muted-foreground hover:text-foreground transition-colors"
            >
              <HugeiconsIcon icon={showAdvancedDb ? ArrowUp01Icon : ArrowDown01Icon} className="size-3" />
              {showAdvancedDb ? "Hide Advanced Tuning" : "Advanced Event Tuning"}
            </button>
          </div>

          {showAdvancedDb && (
            <div className="border border-border/50 bg-secondary/20 rounded-xl p-4 grid grid-cols-2 gap-4 mt-3">
              <div className="space-y-2 col-span-2">
                <p className="text-[10px] text-muted-foreground">
                  Low-level DB pipeline adjustments for high-throughput deployments
                </p>
              </div>
              <div className="space-y-2">
                <label className="text-xs text-muted-foreground">Queue Size (events)</label>
                <Input
                  type="number"
                  value={queueSize}
                  onChange={(e) => { const v = Number(e.target.value); if (!isNaN(v) && v >= 1) setQueueSize(v); }}
                  min={1}
                  max={1_000_000}
                  className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                />
              </div>
              <div className="space-y-2">
                <label className="text-xs text-muted-foreground">Batch Size</label>
                <Input
                  type="number"
                  value={batchSize}
                  onChange={(e) => { const v = Number(e.target.value); if (!isNaN(v) && v >= 1) setBatchSize(v); }}
                  min={1}
                  max={10_000}
                  className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                />
              </div>
              <div className="space-y-2">
                <label className="text-xs text-muted-foreground">Flush Interval (ms)</label>
                <Input
                  type="number"
                  value={flushIntervalMs}
                  onChange={(e) => { const v = Number(e.target.value); if (!isNaN(v) && v >= 50) setFlushIntervalMs(v); }}
                  min={50}
                  max={60_000}
                  className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                />
              </div>
              <div className="space-y-2">
                <label className="text-xs text-muted-foreground">Shutdown Timeout (ms)</label>
                <Input
                  type="number"
                  value={shutdownFlushTimeoutMs}
                  onChange={(e) => { const v = Number(e.target.value); if (!isNaN(v) && v >= 50) setShutdownFlushTimeoutMs(v); }}
                  min={50}
                  max={60_000}
                  className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                />
              </div>
              <div className="space-y-2 col-span-2">
                <label className="text-xs text-muted-foreground">Drop Policy</label>
                <Select value={dropPolicy} onValueChange={setDropPolicy}>
                  <SelectTrigger className="h-9 rounded-lg bg-secondary/50 border-border text-xs font-mono">
                    <SelectValue placeholder="drop_oldest" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="drop_oldest" className="text-xs">drop_oldest</SelectItem>
                    <SelectItem value="drop_newest" className="text-xs">drop_newest</SelectItem>
                    <SelectItem value="block" className="text-xs">block</SelectItem>
                    <SelectItem value="ignore" className="text-xs">ignore</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
          )}

          <div className="border-t border-border/50 pt-5 space-y-4">
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-foreground">Langfuse Adapter</p>
                <p className="text-[10px] text-muted-foreground mt-0.5">
                  Send observability traces to Langfuse and show external link in call detail
                </p>
              </div>
              <div className="flex items-center gap-2 text-xs text-foreground">
                {langfuseMissingRequiredKeys ? (
                  <TooltipProvider>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <HugeiconsIcon icon={Alert01Icon} className="size-3.5 text-orange-500" />
                      </TooltipTrigger>
                      <TooltipContent sideOffset={6}>
                        Langfuse is enabled, but required key(s) are missing.
                      </TooltipContent>
                    </Tooltip>
                  </TooltipProvider>
                ) : null}
                <span>Enabled</span>
                <Switch
                  checked={langfuseEnabled}
                  onCheckedChange={setLangfuseEnabled}
                  aria-label="Enable Langfuse adapter"
                />
              </div>
            </div>

            {langfuseEnabled ? (
              <>
                <div className="space-y-2">
                  <label className="text-xs text-muted-foreground">Langfuse Base URL</label>
                  <Input
                    value={langfuseBaseUrl}
                    onChange={(e) => setLangfuseBaseUrl(e.target.value)}
                    onPaste={handleLangfuseEnvPaste}
                    placeholder="https://cloud.langfuse.com"
                    className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                  />
                </div>

                <div className="grid md:grid-cols-2 gap-3">
                  <div className="space-y-2">
                    <label className="text-xs text-muted-foreground">
                      Public Key{" "}
                      {(observability?.langfuse_has_public_key && !langfusePublicKey) ? (
                        <span className="text-success">(saved)</span>
                      ) : null}
                    </label>
                    <Input
                      type="password"
                      value={langfusePublicKey}
                      onChange={(e) => setLangfusePublicKey(e.target.value)}
                      onPaste={handleLangfuseEnvPaste}
                      placeholder={
                        observability?.langfuse_has_public_key
                          ? "•••••••• (unchanged)"
                          : "pk-lf-..."
                      }
                      className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                    />
                  </div>
                  <div className="space-y-2">
                    <label className="text-xs text-muted-foreground">
                      Secret Key{" "}
                      {(observability?.langfuse_has_secret_key && !langfuseSecretKey) ? (
                        <span className="text-success">(saved)</span>
                      ) : null}
                    </label>
                    <Input
                      type="password"
                      value={langfuseSecretKey}
                      onChange={(e) => setLangfuseSecretKey(e.target.value)}
                      onPaste={handleLangfuseEnvPaste}
                      placeholder={
                        observability?.langfuse_has_secret_key
                          ? "•••••••• (unchanged)"
                          : "sk-lf-..."
                      }
                      className="h-9 rounded-lg bg-secondary/50 border-border font-mono text-xs"
                    />
                  </div>
                </div>

                <div className="flex items-center justify-between border border-border/50 bg-secondary/20 rounded-xl p-4 mt-2">
                  <div>
                    <p className="text-sm font-medium text-foreground">Make Traces Public</p>
                    <p className="text-[10px] text-muted-foreground mt-0.5">
                      Allow viewing observability traces without a Langfuse login
                    </p>
                  </div>
                  <Switch
                    checked={langfuseTracePublic}
                    onCheckedChange={setLangfuseTracePublic}
                  />
                </div>
              </>
            ) : null}

          </div>
        </section>
      </div>
      )}

      {!isHeaderVisible && (
        <div className="fixed bottom-0 left-0 md:left-[240px] right-0 z-50 border-t border-border/70 bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/90">
          <div className="mx-auto flex max-w-[1100px] items-center justify-end gap-3 px-10 py-3">
            {saved && (
              <div className="flex items-center gap-1.5 text-success text-xs font-medium animate-in fade-in duration-300">
                <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-3.5" /> Saved
              </div>
            )}
            <Button
              onClick={saveSettings}
              disabled={saving || loading}
              className="h-8 px-5 text-xs font-medium gap-1.5"
            >
              {saving ? (
                <><Spinner className="size-3.5" /> Saving...</>
              ) : (
                <><HugeiconsIcon icon={SettingDone02Icon} className="size-3.5" /> Save Changes</>
              )}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

export default function SettingsPage() {
  return (
    <Suspense fallback={<div className="flex h-screen items-center justify-center p-4 text-sm text-muted-foreground"><Spinner className="size-5 mr-2" /> Loading settings...</div>}>
      <SettingsPageContent />
    </Suspense>
  );
}
