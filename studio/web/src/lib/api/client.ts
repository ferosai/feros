/**
 * Voice Agent OS API client — type-safe fetch wrapper.
 *
 * Types are auto-generated from the FastAPI OpenAPI spec via:
 *   pnpm sync-api
 *
 * Until the backend is running and types are generated, we use
 * manual type definitions that mirror the Pydantic schemas.
 */

export const API_BASE = process.env.NEXT_PUBLIC_API_URL || "http://localhost:8000";
export const WS_BASE =
  process.env.NEXT_PUBLIC_WS_URL ||
  (process.env.NEXT_PUBLIC_API_URL
    ? process.env.NEXT_PUBLIC_API_URL.replace(/^http/, "ws")
    : "ws://localhost:8000");

// ── Config Types (v3_graph) ──────────────────────────────────────
// Auto-generated via agent.proto
import type {
  AgentGraphDef as AgentGraphConfig,
  NodeDef as AgentNode,
  ToolDef as AgentTool,
  ParamDef as AgentToolParam,
  RecordingConfig,
} from "./agent";

export type { AgentGraphConfig, AgentNode, AgentTool, AgentToolParam, RecordingConfig };

export interface Agent {
  id: string;
  name: string;
  description: string | null;
  status: "draft" | "active" | "paused";
  active_version: number | null;
  phone_number: string | null;
  created_at: string;
  updated_at: string;
  current_config: AgentGraphConfig | null;
  version_count: number;
  greeting_updated?: boolean;
  greeting?: string | null;
  /** Non-empty when the configured TTS model doesn't support the agent's language. */
  model_warning?: string | null;
}

export interface TtsModelSpec {
  provider: string;
  model_id: string;
  label: string;
  supported_languages: string[];
  language_voices: {
    language_code: string;
    voice_id: string;
    voice_label: string;
  }[];
}

export interface SttModelSpec {
  provider: string;
  model_id: string;
  label: string;
  supported_languages: string[];
}

export interface VoiceOption {
  voice_id: string;
  name: string;
  description: string;
  gender: string; // "male" | "female" | ""
  language: string; // ISO 639-1 hint
  preview_url: string;
}

export interface AgentVersion {
  id: string;
  agent_id: string;
  version: number;
  config: AgentGraphConfig;
  change_summary: string | null;
  created_at: string;
}

export interface ImportIssue {
  source: "schema" | "fulfillment";
  code: string;
  path?: string | null;
  message: string;
  severity: "error" | "warning";
  blocking: boolean;
  mappable: boolean;
  suggested_value?: string | null;
}

export interface ImportedConnection {
  provider: string;
  name?: string | null;
  auth_type?: string | null;
  is_default: boolean;
  status: "connected" | "inherited" | "missing";
}

export const FULL_CONFIG_SCHEMA_URL = "https://feros.ai/schemas/agent-config-v1.schema.json" as const;

export interface AgentFullConfig {
  $schema: typeof FULL_CONFIG_SCHEMA_URL;
  name: string;
  description?: string | null;
  config: AgentGraphConfig;
  mermaid_diagram?: string | null;
  connections?: ImportedConnection[];
}

export interface ImportValidationResponse {
  schema_valid: boolean;
  schema_issues: ImportIssue[];
  fulfillable: boolean;
  fulfillment_issues: ImportIssue[];
  suggested_mappings: Record<string, string>;
  normalized_config: AgentGraphConfig;
}

export interface ImportAgentPayload {
  name: string;
  description?: string;
  full_config: AgentFullConfig;
  mapping_mode: "strict" | "map_defaults";
  mappings?: Record<string, string>;
}

export type EvaluationConfigStatus = "active" | "archived";
export type EvaluationRunStatus = "queued" | "running" | "completed" | "failed" | "cancelled";
export type PersonaPreset = "cooperative" | "confused" | "impatient" | "adversarial" | "silent";
export type ScenarioProfile = "balanced" | "happy_path" | "failure_heavy";

export interface GoalTarget {
  id: string;
  title: string;
  description?: string | null;
  success_criteria?: string | null;
}

export interface EvaluationJudgeConfig {
  enabled: boolean;
  rubric_version: string;
}

export interface RubricDimension {
  key: string;
  label: string;
}

export interface RubricPreset {
  id: string;
  display_name: string;
  description?: string | null;
  dimensions: RubricDimension[];
}

export interface RubricPresetListResponse {
  rubrics: RubricPreset[];
}

export interface EvaluationConfigPayload {
  persona_preset: PersonaPreset;
  persona_instructions?: string | null;
  scenario_profile: ScenarioProfile;
  goals: GoalTarget[];
  max_turns: number;
  timeout_seconds: number;
  seed: number;
  run_count: number;
  judge: EvaluationJudgeConfig;
}

export interface EvaluationConfigResponse {
  id: string;
  agent_id: string;
  name: string;
  status: EvaluationConfigStatus;
  latest_version: number;
  created_at: string;
  updated_at: string;
}

export interface EvaluationConfigVersionResponse {
  id: string;
  config_id: string;
  version: number;
  config: EvaluationConfigPayload;
  created_at: string;
}

export interface EvaluationConfigListResponse {
  configs: EvaluationConfigResponse[];
  total: number;
}

export interface EvaluationConfigDetailResponse {
  config: EvaluationConfigResponse;
  versions: EvaluationConfigVersionResponse[];
}

export interface EvaluationRunSummary {
  id: string;
  agent_id: string;
  config_id: string;
  config_version: number;
  target_agent_version: number | null;
  status: EvaluationRunStatus;
  aggregate_score: number | null;
  started_at: string | null;
  ended_at: string | null;
  created_at: string;
}

export interface EvaluationRunListResponse {
  runs: EvaluationRunSummary[];
  total: number;
}

export interface EvaluationRunDetailResponse {
  run: EvaluationRunSummary;
  hard_checks: Record<string, boolean>;
  rubric_scores: Record<string, number>;
  summary: string | null;
}

export interface EvaluationRunsDeleteResponse {
  deleted_count: number;
  skipped_active_count: number;
}

export interface EvaluationRunEvent {
  event_type: string;
  seq_no: number;
  timestamp: string;
  [key: string]: unknown;
}

export interface EvaluationRunEventEnvelope {
  run_id: string;
  event: EvaluationRunEvent;
}

export interface EvaluationRunListParams {
  status?: EvaluationRunStatus;
  config_id?: string;
  started_from?: string;
  started_to?: string;
  skip?: number;
  limit?: number;
}

export interface EvaluationStreamHandlers {
  onEvent?: (event: EvaluationRunEventEnvelope) => void;
  onError?: (error: Event) => void;
}

export interface RenderedPartData {
  kind: "text" | "thinking" | "tool-call" | "tool-return" | "attachment";
  content: string;
  tool_name?: string;
  args?: string;
  // Attachment-specific fields
  file_id?: string;
  filename?: string;
  total_lines?: number;
}

export interface FileAttachment {
  file_id: string;
  filename: string;
  total_lines: number;
}

export interface BuilderMessage {
  id: string;
  role: "user" | "assistant";
  parts?: RenderedPartData[];
  agent_version_id: string | null;
  action_cards?: ActionCard[];
  mermaid_diagram?: string | null;
  imported_connections?: ImportedConnection[];
  created_at: string;
}

export interface BuilderConversation {
  id: string;
  agent_id: string;
  messages: BuilderMessage[];
  created_at: string;
}

export interface ActionCard {
  type: "connect_credential" | "oauth_redirect";
  skill: string;
  title: string;
  description: string;
  help_url: string | null;
}

// Structured streaming event from the builder LLM
export interface StreamPart {
  kind: "part_start" | "part_delta" | "tool_return";
  part_kind?: "text" | "thinking" | "tool-call";
  content?: string;
  tool_name?: string;
  args?: string;
}

// SSE event callbacks for streaming builder
export interface StreamCallbacks {
  onPart?: (part: StreamPart) => void;
  onMermaidStart?: () => void;
  onConfig?: (config: AgentGraphConfig) => void;
  onActionCards?: (cards: ActionCard[]) => void;
  onMermaid?: (diagram: string) => void;
  onProgress?: (data: { step: string; status: string; message: string }) => void;
  onDiff?: (description: string) => void;
  onPreview?: (config: Partial<AgentGraphConfig>) => void;
  onDone?: (data: { version: number | null; change_summary: string | null }) => void;
  onError?: (error: string) => void;
}

export interface CallLog {
  id: string;
  agent_id: string;
  agent_name?: string | null;
  direction: "inbound" | "outbound" | "webrtc";
  caller_number: string | null;
  callee_number: string | null;
  status: string;
  duration_seconds: number | null;
  transcript_json: Record<string, unknown> | null;
  recording_url: string | null;
  outcome: string | null;
  sentiment_score: number | null;
  started_at: string | null;
  ended_at: string | null;
  created_at: string;
}

export interface CallExternalLink {
  adapter: string;
  label: string;
  url: string;
}

export interface CallLogCapabilities {
  has_internal_logs: boolean;
  active_adapters: string[];
  external_links: CallExternalLink[];
}

export interface CallEvent {
  id: string;
  call_id: string;
  session_id: string;
  seq: number;
  event_type: string;
  event_category: string;
  occurred_at: string;
  payload_json: Record<string, unknown>;
}

export interface CallEventListResponse {
  events: CallEvent[];
  total: number;
  skip: number;
  limit: number;
}

// ── Credential Types ─────────────────────────────────────────────

export interface Credential {
  id: string;
  agent_id: string | null;
  name: string;
  provider: string;
  auth_type: string;
  created_at: string;
  updated_at: string;
  /** True when this is a platform-wide default inherited by the agent */
  is_default?: boolean;
}

export interface CredentialCreate {
  name: string;
  provider: string;
  auth_type: string;
  data: Record<string, string>;
}

// ── Skill Types ──────────────────────────────────────────────────

export interface SkillMeta {
  name: string;
  display_name: string;
  description: string;
  auth_type: string;
  category: string;
}

export interface CredentialField {
  key: string;
  label: string;
  type: "password" | "text";
  required: boolean;
  placeholder: string;
  help_text: string;
  help_url?: string;
}

export interface CredentialSchema {
  provider: string;
  display_name: string;
  icon: string;
  auth_type: string;
  fields: CredentialField[];
  header_template: string;
  oauth_available?: boolean;
}

export interface ApiErrorDetail {
  code?: string;
  message?: string;
}

export class ApiError extends Error {
  status: number;
  code?: string;
  detail: unknown;

  constructor(message: string, options: { status: number; code?: string; detail?: unknown }) {
    super(message);
    this.name = "ApiError";
    this.status = options.status;
    this.code = options.code;
    this.detail = options.detail;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

// ── API Client ────────────────────────────────────────────────────

function parseApiErrorDetail(detail: unknown): {
  message?: string;
  code?: string;
} {
  if (!detail || typeof detail !== "object") {
    return {};
  }

  const errorDetail = detail as {
    code?: unknown;
    message?: unknown;
    detail?: unknown;
  };

  if (typeof errorDetail.message === "string" && errorDetail.message) {
    return {
      message: errorDetail.message,
      code: typeof errorDetail.code === "string" ? errorDetail.code : undefined,
    };
  }

  if ("detail" in errorDetail) {
    return parseApiErrorDetail(errorDetail.detail);
  }

  return {};
}

function parseApiError(
  payload: unknown,
  fallbackMessage: string
): { message: string; code?: string; detail: unknown } {
  const detail =
    payload && typeof payload === "object" && "detail" in payload
      ? (payload as { detail?: unknown }).detail
      : payload;

  if (typeof detail === "string" && detail) {
    return { message: detail, detail };
  }

  if (detail && typeof detail === "object") {
    const parsedDetail = parseApiErrorDetail(detail);
    if (parsedDetail.message) {
      return {
        message: parsedDetail.message,
        code: parsedDetail.code,
        detail,
      };
    }
  }

  return { message: fallbackMessage, detail };
}

export function getErrorMessage(error: unknown, fallbackMessage: string): string {
  if (error instanceof ApiError) {
    const parsedDetail = parseApiErrorDetail(error.detail);
    if (parsedDetail.message) {
      return parsedDetail.message;
    }
    if (error.message && error.message !== "[object Object]") {
      return error.message;
    }
  }

  if (error instanceof Error) {
    if (error.message && error.message !== "[object Object]") {
      return error.message;
    }
  }

  const parsedError = parseApiErrorDetail(error);
  if (parsedError.message) {
    return parsedError.message;
  }

  return fallbackMessage;
}

async function buildApiError(res: Response, fallbackMessage: string): Promise<ApiError> {
  const payload = await res.json().catch(() => ({ detail: res.statusText }));
  const parsed = parseApiError(payload, fallbackMessage);
  return new ApiError(parsed.message, {
    status: res.status,
    code: parsed.code,
    detail: parsed.detail,
  });
}

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}/api${path}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      ...options?.headers,
    },
  });

  if (!res.ok) {
    throw await buildApiError(res, `API error: ${res.status}`);
  }

  if (res.status === 204) return undefined as T;
  return res.json();
}

export const api = {
  // Agents
  agents: {
    list: (skip = 0, limit = 50, query?: string) =>
      apiFetch<{ agents: Agent[]; total: number }>(
        `/agents?${new URLSearchParams({
          skip: String(skip),
          limit: String(limit),
          ...(query ? { q: query } : {}),
        }).toString()}`
      ),
    get: (id: string) => apiFetch<Agent>(`/agents/${id}`),
    export: (id: string) =>
      apiFetch<AgentFullConfig>(`/agents/${id}/export`),
    create: (data: { name: string; description?: string }) =>
      apiFetch<Agent>("/agents", {
        method: "POST",
        body: JSON.stringify(data),
      }),
    update: (id: string, data: Partial<Agent>) =>
      apiFetch<Agent>(`/agents/${id}`, {
        method: "PATCH",
        body: JSON.stringify(data),
      }),
    delete: (id: string) => apiFetch<void>(`/agents/${id}`, { method: "DELETE" }),
    versions: (id: string) => apiFetch<AgentVersion[]>(`/agents/${id}/versions`),
    deploy: (id: string, version: number) =>
      apiFetch<Agent>(`/agents/${id}/deploy/${version}`, {
        method: "POST",
      }),
    revert: (id: string, version: number) =>
      apiFetch<AgentVersion>(`/agents/${id}/revert/${version}`, {
        method: "POST",
      }),
    patchConfig: (
      id: string,
      data: {
        language?: string;
        timezone?: string;
        voice_id?: string;
        tts_provider?: string;
        tts_model?: string;
        tool_result_modes?: Record<string, number | null>;
        regenerate_greeting?: boolean;
      }
    ) =>
      apiFetch<Agent>(`/agents/${id}/config`, {
        method: "PATCH",
        body: JSON.stringify(data),
      }),
    getLanguages: () => apiFetch<{ code: string; label: string }[]>("/agents/languages"),
    getTtsModels: (language?: string) =>
      apiFetch<TtsModelSpec[]>(
        `/agents/tts-models${language ? `?language=${encodeURIComponent(language)}` : ""}`
      ),
    importValidate: (config: AgentGraphConfig) =>
      apiFetch<ImportValidationResponse>("/agents/import/validate", {
        method: "POST",
        body: JSON.stringify({ config }),
      }),
    import: (data: ImportAgentPayload) =>
      apiFetch<Agent>("/agents/import", {
        method: "POST",
        body: JSON.stringify(data),
      }),
  },

  // Builder (vibe code)
  builder: {
    getConversation: (agentId: string) =>
      apiFetch<BuilderConversation>(`/agents/${agentId}/builder/conversation`),
    upload: async (
      agentId: string,
      file: File
    ): Promise<{
      file_id: string;
      filename: string;
      total_lines: number;
      text_length: number;
    }> => {
      const form = new FormData();
      form.append("file", file);
      const res = await fetch(`${API_BASE}/api/agents/${agentId}/builder/upload`, {
        method: "POST",
        body: form,
      });
      if (!res.ok) {
        throw await buildApiError(res, `Upload error: ${res.status}`);
      }
      return res.json();
    },
    streamMessage: async (
      agentId: string,
      content: string,
      callbacks: StreamCallbacks,
      attachments?: FileAttachment[]
    ) => {
      const res = await fetch(`${API_BASE}/api/agents/${agentId}/builder/stream`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ content, attachments: attachments ?? [] }),
      });

      if (!res.ok || !res.body) {
        callbacks.onError?.("Failed to connect to builder");
        return;
      }

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        // Parse SSE events from buffer
        const parts = buffer.split("\n\n");
        buffer = parts.pop() || "";

        for (const part of parts) {
          const lines = part.split("\n");
          let eventType = "message";
          let data = "";

          for (const line of lines) {
            if (line.startsWith("event: ")) eventType = line.slice(7);
            else if (line.startsWith("data: ")) data = line.slice(6);
          }

          if (!data) continue;

          try {
            const parsed = JSON.parse(data);
            switch (eventType) {
              case "part":
                callbacks.onPart?.(parsed);
                break;
              case "mermaid_start":
                callbacks.onMermaidStart?.();
                break;
              case "config":
                callbacks.onConfig?.(parsed);
                break;
              case "action_cards":
                callbacks.onActionCards?.(parsed);
                break;
              case "mermaid":
                callbacks.onMermaid?.(parsed);
                break;
              case "progress":
                callbacks.onProgress?.(parsed);
                break;
              case "diff":
                callbacks.onDiff?.(parsed.description);
                break;
              case "preview":
                callbacks.onPreview?.(parsed);
                break;
              case "done":
                callbacks.onDone?.(parsed);
                break;
            }
          } catch {
            // ignore parse errors
          }
        }
      }
    },
  },

  // Credentials
  credentials: {
    list: (agentId: string) =>
      apiFetch<{ credentials: Credential[]; total: number }>(`/agents/${agentId}/credentials`),
    create: (agentId: string, data: CredentialCreate) =>
      apiFetch<Credential>(`/agents/${agentId}/credentials`, {
        method: "POST",
        body: JSON.stringify(data),
      }),
    delete: (agentId: string, credentialId: string) =>
      apiFetch<void>(`/agents/${agentId}/credentials/${credentialId}`, {
        method: "DELETE",
      }),
    update: (
      agentId: string,
      credentialId: string,
      data: { name?: string; data?: Record<string, string> }
    ) =>
      apiFetch<Credential>(`/agents/${agentId}/credentials/${credentialId}`, {
        method: "PUT",
        body: JSON.stringify(data),
      }),
  },

  // Skills / Integrations
  skills: {
    list: () => apiFetch<SkillMeta[]>("/integrations"),
    getCredentialSchema: (skillName: string) =>
      apiFetch<CredentialSchema>(`/integrations/${skillName}/credential-schema`),
  },

  // Integrations (typed with IntegrationSummary)
  integrations: {
    list: () => apiFetch<IntegrationSummary[]>("/integrations"),
    getCredentialSchema: (name: string) =>
      apiFetch<CredentialSchema>(`/integrations/${name}/credential-schema`),
    listDefaultConnections: () =>
      apiFetch<DefaultConnection[]>("/integrations/default-connections"),
    getDefaultConnection: (name: string) =>
      apiFetch<DefaultConnection>(`/integrations/${name}/default-connection`),
    upsertDefaultConnection: (
      name: string,
      data: { auth_type: string; data: Record<string, string> }
    ) =>
      apiFetch<DefaultConnection>(`/integrations/${name}/default-connection`, {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    deleteDefaultConnection: (name: string) =>
      apiFetch<void>(`/integrations/${name}/default-connection`, {
        method: "DELETE",
      }),
  },

  // OAuth
  oauth: {
    getCallbackUrl: () => apiFetch<OAuthCallbackConfig>("/oauth/callback-url"),
    authorize: (skillName: string, agentId: string) =>
      apiFetch<{ authorize_url: string }>(
        `/oauth/${skillName}/authorize?agent_id=${agentId}&origin=${encodeURIComponent(window.location.origin)}`
      ),
    authorizeDefault: (skillName: string) =>
      apiFetch<{ authorize_url: string }>(
        `/oauth/${skillName}/authorize?is_default=true&origin=${encodeURIComponent(window.location.origin)}`
      ),
  },

  // Calls
  calls: {
    list: (agentFilter?: string | string[], skip = 0, limit = 50) =>
      apiFetch<{ calls: CallLog[]; total: number }>(
        `/calls?${(() => {
          const params = new URLSearchParams({
            skip: String(skip),
            limit: String(limit),
          });
          if (typeof agentFilter === "string" && agentFilter) {
            params.set("agent_id", agentFilter);
          } else if (Array.isArray(agentFilter)) {
            for (const id of agentFilter) {
              params.append("agent_ids", id);
            }
          }
          return params.toString();
        })()}`
      ),
    get: (id: string) => apiFetch<CallLog>(`/calls/${id}`),
    getLogCapabilities: (id: string) =>
      apiFetch<CallLogCapabilities>(`/calls/${id}/log-capabilities`),
    getEvents: (id: string, skip = 0, limit = 200) =>
      apiFetch<CallEventListResponse>(`/calls/${id}/events?skip=${skip}&limit=${limit}`),
  },

  evaluations: {
    listRubrics: (agentId: string) =>
      apiFetch<RubricPresetListResponse>(`/agents/${agentId}/evaluations/rubrics`),
    listConfigs: (
      agentId: string,
      params?: { include_archived?: boolean; skip?: number; limit?: number }
    ) => {
      const q = new URLSearchParams();
      if (typeof params?.include_archived === "boolean") {
        q.set("include_archived", String(params.include_archived));
      }
      if (typeof params?.skip === "number") q.set("skip", String(params.skip));
      if (typeof params?.limit === "number") q.set("limit", String(params.limit));
      const suffix = q.toString() ? `?${q.toString()}` : "";
      return apiFetch<EvaluationConfigListResponse>(
        `/agents/${agentId}/evaluations/configs${suffix}`
      );
    },
    createConfig: (agentId: string, data: { name: string; config: EvaluationConfigPayload }) =>
      apiFetch<EvaluationConfigResponse>(`/agents/${agentId}/evaluations/configs`, {
        method: "POST",
        body: JSON.stringify(data),
      }),
    getConfigDetail: (agentId: string, configId: string) =>
      apiFetch<EvaluationConfigDetailResponse>(
        `/agents/${agentId}/evaluations/configs/${configId}`
      ),
    createConfigVersion: (
      agentId: string,
      configId: string,
      data: { config: EvaluationConfigPayload }
    ) =>
      apiFetch<EvaluationConfigVersionResponse>(
        `/agents/${agentId}/evaluations/configs/${configId}/versions`,
        {
          method: "POST",
          body: JSON.stringify(data),
        }
      ),
    runConfig: (
      agentId: string,
      configId: string,
      data: { config_version?: number | null },
      idempotencyKey?: string
    ) =>
      apiFetch<EvaluationRunSummary>(`/agents/${agentId}/evaluations/configs/${configId}/run`, {
        method: "POST",
        body: JSON.stringify(data),
        headers: idempotencyKey ? { "Idempotency-Key": idempotencyKey } : {},
      }),
    listRuns: (agentId: string, params?: EvaluationRunListParams) => {
      const q = new URLSearchParams();
      if (params?.status) q.set("status", params.status);
      if (params?.config_id) q.set("config_id", params.config_id);
      if (params?.started_from) q.set("started_from", params.started_from);
      if (params?.started_to) q.set("started_to", params.started_to);
      if (typeof params?.skip === "number") q.set("skip", String(params.skip));
      if (typeof params?.limit === "number") q.set("limit", String(params.limit));
      const suffix = q.toString() ? `?${q.toString()}` : "";
      return apiFetch<EvaluationRunListResponse>(`/agents/${agentId}/evaluations/runs${suffix}`);
    },
    getRunDetail: (agentId: string, runId: string) =>
      apiFetch<EvaluationRunDetailResponse>(`/agents/${agentId}/evaluations/runs/${runId}`),
    deleteRun: (agentId: string, runId: string) =>
      apiFetch<EvaluationRunsDeleteResponse>(`/agents/${agentId}/evaluations/runs/${runId}`, {
        method: "DELETE",
      }),
    clearRunHistory: (agentId: string) =>
      apiFetch<EvaluationRunsDeleteResponse>(`/agents/${agentId}/evaluations/runs`, {
        method: "DELETE",
      }),
    cancelRun: (agentId: string, runId: string, idempotencyKey?: string) =>
      apiFetch<EvaluationRunSummary>(`/agents/${agentId}/evaluations/runs/${runId}/cancel`, {
        method: "POST",
        headers: idempotencyKey ? { "Idempotency-Key": idempotencyKey } : {},
      }),
    rerunRun: (
      agentId: string,
      runId: string,
      data: { seed_override?: number | null },
      idempotencyKey?: string
    ) =>
      apiFetch<EvaluationRunSummary>(`/agents/${agentId}/evaluations/runs/${runId}/rerun`, {
        method: "POST",
        body: JSON.stringify(data),
        headers: idempotencyKey ? { "Idempotency-Key": idempotencyKey } : {},
      }),
    streamRunEvents: (
      agentId: string,
      runId: string,
      fromSeq: number,
      handlers: EvaluationStreamHandlers
    ) => {
      const source = new EventSource(
        `${API_BASE}/api/agents/${agentId}/evaluations/runs/${runId}/events?from_seq=${fromSeq}`
      );
      source.addEventListener("run_event", (event) => {
        try {
          const payload = JSON.parse(
            (event as MessageEvent<string>).data
          ) as EvaluationRunEventEnvelope;
          handlers.onEvent?.(payload);
        } catch {
          // Ignore malformed payloads.
        }
      });
      source.onerror = (error) => {
        handlers.onError?.(error);
      };
      return () => source.close();
    },
  },

  // Settings
  settings: {
    getLLM: () => apiFetch<LLMSettings>("/settings/llm"),
    updateLLM: (data: LLMSettingsUpdate) =>
      apiFetch<LLMSettings>("/settings/llm", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    getVoiceLLM: () => apiFetch<LLMSettings>("/settings/voice-llm"),
    updateVoiceLLM: (data: LLMSettingsUpdate) =>
      apiFetch<LLMSettings>("/settings/voice-llm", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    getNativeMultimodal: () => apiFetch<LLMSettings>("/settings/native-multimodal"),
    updateNativeMultimodal: (data: LLMSettingsUpdate) =>
      apiFetch<LLMSettings>("/settings/native-multimodal", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    getSTT: () => apiFetch<VoiceProviderSettings>("/settings/stt"),
    updateSTT: (data: VoiceProviderUpdate) =>
      apiFetch<VoiceProviderSettings>("/settings/stt", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    getTTS: () => apiFetch<VoiceProviderSettings>("/settings/tts"),
    getTtsVoices: (language?: string, provider?: string) => {
      const p = new URLSearchParams();
      if (language) p.set("language", language);
      if (provider) p.set("provider", provider);
      const qs = p.toString();
      return apiFetch<VoiceOption[]>(`/settings/tts/voices${qs ? `?${qs}` : ""}`);
    },
    getTtsCatalog: (provider?: string) =>
      apiFetch<SttModelSpec[]>(
        `/settings/tts-catalog${provider ? `?provider=${encodeURIComponent(provider)}` : ""}`
      ),
    getSttCatalog: (provider?: string) =>
      apiFetch<SttModelSpec[]>(
        `/settings/stt-catalog${provider ? `?provider=${encodeURIComponent(provider)}` : ""}`
      ),
    updateTTS: (data: VoiceProviderUpdate) =>
      apiFetch<VoiceProviderSettings>("/settings/tts", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    getTelephony: () => apiFetch<TelephonySettings>("/settings/telephony"),
    updateTelephony: (data: TelephonySettingsUpdate) =>
      apiFetch<TelephonySettings>("/settings/telephony", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
    getObservability: () => apiFetch<ObservabilitySettings>("/settings/observability"),
    updateObservability: (data: ObservabilitySettingsUpdate) =>
      apiFetch<ObservabilitySettings>("/settings/observability", {
        method: "PUT",
        body: JSON.stringify(data),
      }),
  },

  // OAuth Apps (platform-level registrations)
  oauthApps: {
    list: () => apiFetch<OAuthApp[]>("/oauth-apps"),
    upsert: (data: OAuthAppCreate) =>
      apiFetch<OAuthApp>("/oauth-apps", {
        method: "POST",
        body: JSON.stringify(data),
      }),
    patch: (
      integrationName: string,
      data: { client_id?: string; client_secret?: string; enabled?: boolean }
    ) =>
      apiFetch<OAuthApp>(`/oauth-apps/${integrationName}`, {
        method: "PATCH",
        body: JSON.stringify(data),
      }),
    delete: (integrationName: string) =>
      apiFetch<void>(`/oauth-apps/${integrationName}`, { method: "DELETE" }),
  },

  // Phone Numbers
  phoneNumbers: {
    list: () => apiFetch<PhoneNumberListResponse>("/phone-numbers"),
    fetch: (data: FetchNumbersRequest) =>
      apiFetch<FetchNumbersResponse>("/phone-numbers/fetch", {
        method: "POST",
        body: JSON.stringify(data),
      }),
    importSelected: (data: ImportNumbersRequest) =>
      apiFetch<PhoneNumberListResponse>("/phone-numbers/import-selected", {
        method: "POST",
        body: JSON.stringify(data),
      }),
    assign: (id: string, data: AssignPhoneNumberRequest) =>
      apiFetch<PhoneNumber>(`/phone-numbers/${id}/assign`, {
        method: "PATCH",
        body: JSON.stringify(data),
      }),
    delete: (id: string) => apiFetch<void>(`/phone-numbers/${id}`, { method: "DELETE" }),
  },
};

// ── Settings Types ──────────────────────────────────────────────

export interface LLMProviderOption {
  value: string;
  label: string;
  description: string;
}

export interface LLMSettings {
  provider: string;
  model: string;
  base_url: string;
  has_api_key: boolean;
  temperature: number;
  max_tokens: number;
  supported_providers: LLMProviderOption[];
  /** Provider slug → has_api_key for all providers that have saved credentials. */
  all_credentials: Record<string, boolean>;
  /** Non-secret config per provider slug (model, base_url, …) for pre-population. */
  all_configs: Record<string, Record<string, string>>;
}

export interface LLMSettingsUpdate {
  provider: string;
  model: string;
  base_url?: string;
  api_key?: string;
  temperature?: number;
  max_tokens?: number;
}

// ── Generic Voice Provider Types (STT + TTS) ────────────────────

export interface VoiceProviderField {
  key: string;
  label: string;
  placeholder: string;
  is_secret: boolean;
}

export interface VoiceProviderOption {
  value: string;
  label: string;
  description: string;
  default_base_url: string;
  docs_url: string;
  fields: VoiceProviderField[];
}

export interface VoiceProviderSettings {
  provider: string;
  base_url: string;
  has_api_key: boolean;
  config: Record<string, string>;
  supported_providers: VoiceProviderOption[];
  /** Provider slug → has_api_key for all providers that have saved credentials. */
  all_credentials: Record<string, boolean>;
  /** Non-secret config per provider slug (model, voice_id, etc.) for pre-population. */
  all_configs: Record<string, Record<string, string>>;
}

export interface VoiceProviderUpdate {
  provider: string;
  base_url: string;
  config: Record<string, string>;
}

// ── Telephony Settings Types ──────────────────────────────────────

export interface TelephonySettings {
  voice_server_url: string;
}

export interface TelephonySettingsUpdate {
  voice_server_url?: string;
}

export interface ObservabilitySettings {
  db_events_enabled: boolean;
  langfuse_enabled: boolean;
  langfuse_base_url: string;
  langfuse_has_public_key: boolean;
  langfuse_has_secret_key: boolean;
  langfuse_trace_public: boolean;
  queue_size: number;
  batch_size: number;
  flush_interval_ms: number;
  shutdown_flush_timeout_ms: number;
  drop_policy: string;
  db_categories: string[];
  db_event_types: string[];
}

export interface ObservabilitySettingsUpdate {
  db_events_enabled: boolean;
  langfuse_enabled: boolean;
  langfuse_base_url: string;
  langfuse_public_key?: string;
  langfuse_secret_key?: string;
  langfuse_trace_public: boolean;
  queue_size: number;
  batch_size: number;
  flush_interval_ms: number;
  shutdown_flush_timeout_ms: number;
  drop_policy: string;
  db_categories: string[];
  db_event_types: string[];
}

// ── Phone Number Types ───────────────────────────────────────────

export interface PhoneNumber {
  id: string;
  provider: "twilio" | "telnyx";
  phone_number: string;
  provider_sid: string | null;
  friendly_name: string | null;
  agent_id: string | null;
  voice_server_url: string | null;
  telnyx_connection_id: string | null;
  is_active: boolean;
  has_credentials: boolean;
  created_at: string;
  updated_at: string;
}

export interface PhoneNumberListResponse {
  phone_numbers: PhoneNumber[];
  total: number;
}

export interface AssignPhoneNumberRequest {
  agent_id: string | null;
  telnyx_connection_id?: string | null;
}

// ── Per-number credential import types ──────────────────────────

export interface FetchNumbersRequest {
  provider: "twilio" | "telnyx";
  twilio_account_sid?: string;
  twilio_auth_token?: string;
  telnyx_api_key?: string;
}

export interface ProviderNumber {
  phone_number: string;
  provider_sid: string;
  friendly_name: string;
  locality: string;
  region: string;
  number_type: string;
  already_imported: boolean;
  disabled_reason: string;
}

export interface FetchNumbersResponse {
  numbers: ProviderNumber[];
}

export interface ImportNumbersRequest {
  provider: "twilio" | "telnyx";
  twilio_account_sid?: string;
  twilio_auth_token?: string;
  telnyx_api_key?: string;
  selected_numbers: string[];
}

// ── OAuth App Types ──────────────────────────────────────────────

export interface OAuthApp {
  id: string;
  integration_name: string;
  client_id: string;
  enabled: boolean;
}

export interface OAuthAppCreate {
  integration_name: string;
  client_id: string;
  client_secret: string;
  enabled?: boolean;
}

export interface OAuthCallbackConfig {
  callback_url: string;
}

// ── Integration Summary Type ─────────────────────────────────────

export interface IntegrationSummary {
  name: string;
  display_name: string;
  description: string;
  categories: string[];
  icon: string;
  auth_type: string;
  supports_byok: boolean;
}

// ── Default Connection Types ──────────────────────────────────────

export interface DefaultConnection {
  id: string;
  provider: string;
  auth_type: string;
  is_configured: boolean;
  created_at: string;
  updated_at: string;
}
