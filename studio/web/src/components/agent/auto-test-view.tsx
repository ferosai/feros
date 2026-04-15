"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  ArrowLeft01Icon,
  Cancel01Icon,
  CancelCircleIcon,
  CheckmarkCircle02Icon,
  Delete02Icon,
  GitBranchIcon,
  HardDriveUploadIcon,
  PanelLeftIcon,
  PlayIcon,
  RedoIcon,
  SquareIcon,
  UndoIcon,
} from "@hugeicons/core-free-icons";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  type Agent,
  type EvaluationConfigDetailResponse,
  type EvaluationConfigPayload,
  type EvaluationConfigResponse,
  type EvaluationRunDetailResponse,
  type EvaluationRunEvent,
  type EvaluationRunSummary,
  type GoalTarget,
  type PersonaPreset,
  type RubricPreset,
  type ScenarioProfile,
} from "@/lib/api/client";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectItemText,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";

interface AutoTestViewProps {
  agentId: string;
  agent: Agent | null;
  activeTab: AutoTab;
  onTabChange: (tab: AutoTab) => void;
}

type AutoTab = "configs" | "runs";
type RunDetailTab = "result" | "conversation" | "events";

interface GoalDraft {
  localId: string;
  id: string;
  title: string;
  description: string;
  success_criteria: string;
}

interface FormState {
  name: string;
  persona_preset: PersonaPreset;
  persona_instructions: string;
  scenario_profile: ScenarioProfile;
  max_turns: string;
  timeout_seconds: string;
  seed: string;
  run_count: string;
  judge_enabled: boolean;
  rubric_version: string;
  goals: GoalDraft[];
}

function newClientId(prefix: string): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Math.random().toString(36).slice(2)}`;
}

function isServerRunId(runId: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(runId);
}

function _isTerminalRunStatus(status: EvaluationRunSummary["status"]): boolean {
  return status === "completed" || status === "failed" || status === "cancelled";
}

function makeGoalDraft(goal?: GoalTarget): GoalDraft {
  return {
    localId: newClientId("goal"),
    id: goal?.id ?? "",
    title: goal?.title ?? "",
    description: goal?.description ?? "",
    success_criteria: goal?.success_criteria ?? "",
  };
}

function defaultFormState(): FormState {
  return {
    name: "",
    persona_preset: "cooperative",
    persona_instructions: "",
    scenario_profile: "balanced",
    max_turns: "12",
    timeout_seconds: "180",
    seed: "42",
    run_count: "1",
    judge_enabled: true,
    rubric_version: "support_quality",
    goals: [],
  };
}

function formFromPayload(name: string, payload: EvaluationConfigPayload): FormState {
  return {
    name,
    persona_preset: payload.persona_preset,
    persona_instructions: payload.persona_instructions ?? "",
    scenario_profile: payload.scenario_profile,
    max_turns: String(payload.max_turns),
    timeout_seconds: String(payload.timeout_seconds),
    seed: String(payload.seed),
    run_count: String(payload.run_count),
    judge_enabled: payload.judge.enabled,
    rubric_version: payload.judge.rubric_version,
    goals: (payload.goals ?? []).map((goal) => makeGoalDraft(goal)),
  };
}

function payloadFromForm(form: FormState): EvaluationConfigPayload {
  return {
    persona_preset: form.persona_preset,
    persona_instructions: form.persona_instructions.trim() || null,
    scenario_profile: form.scenario_profile,
    max_turns: Number(form.max_turns),
    timeout_seconds: Number(form.timeout_seconds),
    seed: Number(form.seed),
    run_count: Number(form.run_count),
    goals: form.goals
      .map((goal) => ({
        id: goal.id.trim(),
        title: goal.title.trim(),
        description: goal.description.trim() || null,
        success_criteria: goal.success_criteria.trim() || null,
      }))
      .filter((goal) => goal.id && goal.title),
    judge: {
      enabled: form.judge_enabled,
      rubric_version: form.rubric_version.trim() || "v1",
    },
  };
}

function normalizePayloadForComparison(payload: EvaluationConfigPayload): EvaluationConfigPayload {
  return {
    ...payload,
    run_count: payload.run_count ?? 1,
  };
}

function canonicalizeForCompare(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(canonicalizeForCompare);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([key, nested]) => [key, canonicalizeForCompare(nested)])
    );
  }
  return value;
}

function _statusVariant(
  status: EvaluationRunSummary["status"]
): "success" | "secondary" | "destructive" | "outline" {
  if (status === "completed") return "success";
  if (status === "failed" || status === "cancelled") return "destructive";
  if (status === "running") return "secondary";
  return "outline";
}

function statusTextClass(status: EvaluationRunSummary["status"]): string {
  if (status === "completed") return "text-success";
  if (status === "failed" || status === "cancelled") return "text-destructive";
  if (status === "running") return "text-primary";
  return "text-muted-foreground";
}

function formatAggregateScore(score: number | null): string {
  if (score == null) return "-/100";
  const normalized = Number.isInteger(score) ? `${score}` : score.toFixed(2);
  return `${normalized}/100`;
}

function formatDate(value: string | null): string {
  if (!value) return "-";
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return "-";
  return parsed.toLocaleString();
}

function isValidNumericForm(form: FormState): boolean {
  const maxTurns = Number(form.max_turns);
  const timeout = Number(form.timeout_seconds);
  const seed = Number(form.seed);
  const runCount = Number(form.run_count);
  if (!form.name.trim()) return false;
  if (!Number.isInteger(maxTurns) || maxTurns < 1 || maxTurns > 100) return false;
  if (!Number.isInteger(timeout) || timeout < 10 || timeout > 3600) return false;
  if (!Number.isInteger(seed) || seed < 0) return false;
  if (!Number.isInteger(runCount) || runCount < 1 || runCount > 20) return false;
  return true;
}

function humanizeLabel(value: string): string {
  const labelOverrides: Record<string, string> = {
    persona_adherence: "Persona Fit",
  };
  if (labelOverrides[value]) {
    return labelOverrides[value];
  }
  return value
    .split("_")
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

export default function AutoTestView({
  agentId,
  agent,
  activeTab,
  onTabChange,
}: AutoTestViewProps) {
  const [configs, setConfigs] = useState<EvaluationConfigResponse[]>([]);
  const [runs, setRuns] = useState<EvaluationRunSummary[]>([]);
  const [loadingConfigs, setLoadingConfigs] = useState(false);
  const [loadingRuns, setLoadingRuns] = useState(false);
  const [loadingRubrics, setLoadingRubrics] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rubrics, setRubrics] = useState<RubricPreset[]>([]);

  const [selectedConfigId, setSelectedConfigId] = useState<string | null>(null);
  const [selectedConfigDetail, setSelectedConfigDetail] =
    useState<EvaluationConfigDetailResponse | null>(null);
  const [loadedConfigVersion, setLoadedConfigVersion] = useState<number | null>(null);
  const [creatingConfig, setCreatingConfig] = useState(false);
  const [form, setForm] = useState<FormState>(() => defaultFormState());
  const [saving, setSaving] = useState(false);
  const [runningNow, setRunningNow] = useState(false);
  const [runningConfigId, setRunningConfigId] = useState<string | null>(null);
  const [runStartedToast, setRunStartedToast] = useState<string | null>(null);

  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [selectedRunDetail, setSelectedRunDetail] = useState<EvaluationRunDetailResponse | null>(
    null
  );
  const [runEventsById, setRunEventsById] = useState<Record<string, EvaluationRunEvent[]>>({});
  const [loadingRunDetail, setLoadingRunDetail] = useState(false);
  const [runActionLoading, setRunActionLoading] = useState<Record<string, boolean>>({});
  const [clearDialogOpen, setClearDialogOpen] = useState(false);
  const [removeDialogOpen, setRemoveDialogOpen] = useState(false);
  const [configListHidden, setConfigListHidden] = useState(false);
  const [runListHidden, setRunListHidden] = useState(false);
  const [runDetailTab, setRunDetailTab] = useState<RunDetailTab>("result");
  const [pendingRunFocusId, setPendingRunFocusId] = useState<string | null>(null);

  const streamCloseRef = useRef<(() => void) | null>(null);

  const hasActiveRuns = useMemo(
    () => runs.some((run) => run.status === "queued" || run.status === "running"),
    [runs]
  );
  const configNameById = useMemo(
    () => Object.fromEntries(configs.map((config) => [config.id, config.name] as const)),
    [configs]
  );

  const selectedRunEvents = useMemo(
    () => (selectedRunId ? (runEventsById[selectedRunId] ?? []) : []),
    [runEventsById, selectedRunId]
  );
  const selectedConversation = useMemo(
    () =>
      selectedRunEvents
        .filter(
          (event) =>
            event.event_type === "caller_utterance" || event.event_type === "assistant_reply"
        )
        .map((event) => ({
          role: event.event_type === "caller_utterance" ? "test_agent" : "target_agent",
          text: typeof event.text === "string" ? event.text : "",
          seqNo: event.seq_no,
          turnId: typeof event.turn_id === "number" ? event.turn_id : null,
        })),
    [selectedRunEvents]
  );
  const loadedVersionPayload = useMemo(() => {
    if (!selectedConfigDetail || loadedConfigVersion == null) return null;
    const match = selectedConfigDetail.versions.find(
      (version) => version.version === loadedConfigVersion
    );
    return match?.config ?? null;
  }, [loadedConfigVersion, selectedConfigDetail]);
  const isExistingConfigMode = !creatingConfig && Boolean(selectedConfigId);
  const isConfigFormDirty = useMemo(() => {
    if (!isExistingConfigMode || !loadedVersionPayload) return false;
    const current = JSON.stringify(
      canonicalizeForCompare(normalizePayloadForComparison(payloadFromForm(form)))
    );
    const baseline = JSON.stringify(
      canonicalizeForCompare(normalizePayloadForComparison(loadedVersionPayload))
    );
    return current !== baseline;
  }, [form, isExistingConfigMode, loadedVersionPayload]);
  const canSaveVersion =
    isExistingConfigMode && isConfigFormDirty && !saving && !runningNow && isValidNumericForm(form);
  const runPrimaryLabel = isConfigFormDirty ? "Save and Run" : "Run";

  const refreshConfigs = useCallback(async () => {
    setLoadingConfigs(true);
    try {
      const response = await api.evaluations.listConfigs(agentId, {
        skip: 0,
        limit: 100,
      });
      setConfigs(response.configs);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load configurations");
    } finally {
      setLoadingConfigs(false);
    }
  }, [agentId]);

  const refreshRubrics = useCallback(async () => {
    setLoadingRubrics(true);
    try {
      const response = await api.evaluations.listRubrics(agentId);
      setRubrics(response.rubrics);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load rubrics");
    } finally {
      setLoadingRubrics(false);
    }
  }, [agentId]);

  const refreshRuns = useCallback(
    async (options?: { silent?: boolean }) => {
      const silent = options?.silent ?? false;
      if (!silent) {
        setLoadingRuns(true);
      }
      try {
        const response = await api.evaluations.listRuns(agentId, {
          skip: 0,
          limit: 100,
        });
        setRuns(response.runs);
        setSelectedRunDetail((previous) => {
          if (!previous) return previous;
          const matched = response.runs.find((run) => run.id === previous.run.id);
          return matched ? { ...previous, run: matched } : previous;
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to load run history");
      } finally {
        if (!silent) {
          setLoadingRuns(false);
        }
      }
    },
    [agentId]
  );

  useEffect(() => {
    void refreshRubrics();
    void refreshConfigs();
    void refreshRuns();
  }, [refreshConfigs, refreshRubrics, refreshRuns]);

  useEffect(() => {
    if (!hasActiveRuns) return;
    const timer = setInterval(() => {
      void refreshRuns({ silent: true });
    }, 5000);
    return () => clearInterval(timer);
  }, [hasActiveRuns, refreshRuns]);

  useEffect(() => {
    return () => {
      streamCloseRef.current?.();
      streamCloseRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (!runStartedToast) return;
    const timer = window.setTimeout(() => setRunStartedToast(null), 2500);
    return () => window.clearTimeout(timer);
  }, [runStartedToast]);

  const selectConfig = useCallback(
    async (configId: string) => {
      setError(null);
      setCreatingConfig(false);
      setSelectedConfigId(configId);
      try {
        const detail = await api.evaluations.getConfigDetail(agentId, configId);
        setSelectedConfigDetail(detail);
        const latestVersion = [...detail.versions].sort((a, b) => b.version - a.version)[0];
        if (latestVersion) {
          setLoadedConfigVersion(latestVersion.version);
          setForm(formFromPayload(detail.config.name, latestVersion.config));
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to load configuration details");
      }
    },
    [agentId]
  );

  const startCreateConfig = useCallback(() => {
    setCreatingConfig(true);
    setSelectedConfigId(null);
    setSelectedConfigDetail(null);
    setLoadedConfigVersion(null);
    setForm(defaultFormState());
    setError(null);
  }, []);

  const addGoal = useCallback(() => {
    setForm((previous) => ({
      ...previous,
      goals: [...previous.goals, makeGoalDraft()],
    }));
  }, []);

  const removeGoal = useCallback((localId: string) => {
    setForm((previous) => ({
      ...previous,
      goals: previous.goals.filter((goal) => goal.localId !== localId),
    }));
  }, []);

  const updateGoal = useCallback((localId: string, field: keyof GoalDraft, value: string) => {
    setForm((previous) => ({
      ...previous,
      goals: previous.goals.map((goal) =>
        goal.localId === localId ? { ...goal, [field]: value } : goal
      ),
    }));
  }, []);

  const runFromConfig = useCallback(
    async (config: EvaluationConfigResponse, configVersion?: number) => {
      const tempId = newClientId("run-temp");
      const optimistic: EvaluationRunSummary = {
        id: tempId,
        agent_id: agentId,
        config_id: config.id,
        config_version: configVersion ?? config.latest_version,
        target_agent_version: agent?.active_version ?? null,
        status: "queued",
        aggregate_score: null,
        started_at: null,
        ended_at: null,
        created_at: new Date().toISOString(),
      };
      setRunningConfigId(config.id);
      onTabChange("runs");
      setRunListHidden(false);
      setSelectedRunId(tempId);
      setSelectedRunDetail(null);
      setRuns((previous) => [optimistic, ...previous]);

      try {
        const created = await api.evaluations.runConfig(
          agentId,
          config.id,
          { config_version: configVersion ?? null },
          newClientId("idem-run")
        );
        setRuns((previous) => [created, ...previous.filter((run) => run.id !== tempId)]);
        setSelectedRunId(created.id);
        setRunDetailTab("conversation");
        setPendingRunFocusId(created.id);
        setRunStartedToast("Run queued. Switched to History.");
      } catch (err) {
        setRuns((previous) => previous.filter((run) => run.id !== tempId));
        setError(err instanceof Error ? err.message : "Failed to start run");
      } finally {
        setRunningConfigId((previous) => (previous === config.id ? null : previous));
      }
    },
    [agent?.active_version, agentId, onTabChange]
  );

  const saveConfig = useCallback(
    async (runImmediately: boolean) => {
      if (!isValidNumericForm(form)) {
        setError("Please provide valid values before saving.");
        return;
      }
      setError(null);
      if (runImmediately) setRunningNow(true);
      else setSaving(true);

      try {
        const payload = payloadFromForm(form);
        if (selectedConfigId && !creatingConfig) {
          const version = await api.evaluations.createConfigVersion(agentId, selectedConfigId, {
            config: payload,
          });
          setLoadedConfigVersion(version.version);
          const detail = await api.evaluations.getConfigDetail(agentId, selectedConfigId);
          setSelectedConfigDetail(detail);
          await refreshConfigs();
          if (runImmediately) {
            const record = configs.find((item) => item.id === selectedConfigId);
            if (record) {
              await runFromConfig(record, version.version);
            }
          }
        } else {
          const created = await api.evaluations.createConfig(agentId, {
            name: form.name.trim(),
            config: payload,
          });
          setCreatingConfig(false);
          setSelectedConfigId(created.id);
          setLoadedConfigVersion(null);
          await refreshConfigs();
          await selectConfig(created.id);
          if (runImmediately) {
            await runFromConfig(created);
          }
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to save configuration");
      } finally {
        setSaving(false);
        setRunningNow(false);
      }
    },
    [
      agentId,
      configs,
      creatingConfig,
      form,
      refreshConfigs,
      runFromConfig,
      selectConfig,
      selectedConfigId,
    ]
  );

  const setRunLoading = useCallback((runId: string, loading: boolean) => {
    setRunActionLoading((previous) => ({ ...previous, [runId]: loading }));
  }, []);

  const rerun = useCallback(
    async (run: EvaluationRunSummary) => {
      const tempId = newClientId("run-rerun");
      const optimistic: EvaluationRunSummary = {
        ...run,
        id: tempId,
        status: "queued",
        aggregate_score: null,
        started_at: null,
        ended_at: null,
        created_at: new Date().toISOString(),
      };

      setRunLoading(run.id, true);
      onTabChange("runs");
      setRunListHidden(false);
      setSelectedRunId(tempId);
      setSelectedRunDetail(null);
      setRuns((previous) => [optimistic, ...previous]);
      try {
        const created = await api.evaluations.rerunRun(
          agentId,
          run.id,
          {},
          newClientId("idem-rerun")
        );
        setRuns((previous) => [created, ...previous.filter((item) => item.id !== tempId)]);
        setSelectedRunId(created.id);
        setRunDetailTab("conversation");
        setPendingRunFocusId(created.id);
        setRunStartedToast("Run queued. Switched to History.");
      } catch (err) {
        setRuns((previous) => previous.filter((item) => item.id !== tempId));
        setError(err instanceof Error ? err.message : "Failed to rerun");
      } finally {
        setRunLoading(run.id, false);
      }
    },
    [agentId, setRunLoading, onTabChange]
  );

  const cancelRun = useCallback(
    async (run: EvaluationRunSummary) => {
      setRunLoading(run.id, true);
      const previousStatus = run.status;
      setRuns((previous) =>
        previous.map((item) => (item.id === run.id ? { ...item, status: "cancelled" } : item))
      );

      try {
        const updated = await api.evaluations.cancelRun(
          agentId,
          run.id,
          newClientId("idem-cancel")
        );
        setRuns((previous) => previous.map((item) => (item.id === run.id ? updated : item)));
      } catch (err) {
        setRuns((previous) =>
          previous.map((item) => (item.id === run.id ? { ...item, status: previousStatus } : item))
        );
        setError(err instanceof Error ? err.message : "Failed to cancel run");
      } finally {
        setRunLoading(run.id, false);
      }
    },
    [agentId, setRunLoading]
  );

  const removeRun = useCallback(
    async (run: EvaluationRunSummary) => {
      setRunLoading(run.id, true);
      const previousRuns = runs;
      setRuns((current) => current.filter((item) => item.id !== run.id));
      setRunEventsById((previous) => {
        if (!previous[run.id]) return previous;
        const next = { ...previous };
        delete next[run.id];
        return next;
      });
      if (selectedRunId === run.id) {
        setSelectedRunId(null);
        setSelectedRunDetail(null);
      }

      try {
        await api.evaluations.deleteRun(agentId, run.id);
        await refreshRuns();
      } catch (err) {
        setRuns(previousRuns);
        setError(err instanceof Error ? err.message : "Failed to remove run");
      } finally {
        setRunLoading(run.id, false);
        setRemoveDialogOpen(false);
      }
    },
    [agentId, refreshRuns, runs, selectedRunId, setRunLoading]
  );

  const clearHistory = useCallback(async () => {
    setLoadingRuns(true);
    try {
      const response = await api.evaluations.clearRunHistory(agentId);
      setRuns((previous) =>
        previous.filter((run) => run.status === "queued" || run.status === "running")
      );
      setRunEventsById({});
      setSelectedRunId((previous) => {
        if (!previous) return previous;
        const stillExists = runs.some(
          (run) => run.id === previous && (run.status === "queued" || run.status === "running")
        );
        return stillExists ? previous : null;
      });
      setSelectedRunDetail((previous) => {
        if (!previous) return previous;
        if (previous.run.status === "queued" || previous.run.status === "running") {
          return previous;
        }
        return null;
      });
      if (response.skipped_active_count > 0) {
        setError(
          `Cleared ${response.deleted_count} runs. ${response.skipped_active_count} active run(s) were kept.`
        );
      } else {
        setError(null);
      }
      await refreshRuns();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to clear run history");
    } finally {
      setLoadingRuns(false);
      setClearDialogOpen(false);
    }
  }, [agentId, refreshRuns, runs]);

  const openRunDetail = useCallback(
    async (runId: string) => {
      setError(null);
      setSelectedRunId(runId);
      setLoadingRunDetail(true);
      streamCloseRef.current?.();
      streamCloseRef.current = null;

      try {
        const detail = await api.evaluations.getRunDetail(agentId, runId);
        setSelectedRunDetail(detail);

        const existingEvents = runEventsById[runId] ?? [];
        let maxSeq =
          existingEvents.length > 0 ? existingEvents[existingEvents.length - 1].seq_no : 0;

        streamCloseRef.current = api.evaluations.streamRunEvents(agentId, runId, maxSeq, {
          onEvent: (envelope) => {
            setRunEventsById((previous) => {
              const runEvents = previous[runId] ?? [];
              if (runEvents.some((event) => event.seq_no === envelope.event.seq_no)) {
                return previous;
              }
              const next = [...runEvents, envelope.event].sort((a, b) => a.seq_no - b.seq_no);
              maxSeq = next.length > 0 ? next[next.length - 1].seq_no : maxSeq;
              return { ...previous, [runId]: next };
            });

            if (
              envelope.event.event_type === "run_finished" ||
              envelope.event.event_type === "run_failed"
            ) {
              streamCloseRef.current?.();
              streamCloseRef.current = null;
              void refreshRuns();
              void api.evaluations
                .getRunDetail(agentId, runId)
                .then(setSelectedRunDetail)
                .catch(() => {});
            }
          },
          onError: () => {
            streamCloseRef.current?.();
            streamCloseRef.current = null;
          },
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to load run detail");
      } finally {
        setLoadingRunDetail(false);
      }
    },
    [agentId, refreshRuns, runEventsById]
  );

  useEffect(() => {
    if (!pendingRunFocusId) return;
    const timer = window.setTimeout(() => {
      void openRunDetail(pendingRunFocusId);
      setPendingRunFocusId(null);
    }, 250);
    return () => window.clearTimeout(timer);
  }, [openRunDetail, pendingRunFocusId]);

  useEffect(() => {
    if (configs.length === 0) {
      setSelectedConfigId(null);
      setSelectedConfigDetail(null);
      setLoadedConfigVersion(null);
      return;
    }
    if (!selectedConfigId && !creatingConfig) {
      const first = configs[0];
      setSelectedConfigId(first.id);
      void selectConfig(first.id);
    }
  }, [configs, creatingConfig, selectConfig, selectedConfigId]);

  useEffect(() => {
    if (runs.length === 0) {
      setSelectedRunId(null);
      setSelectedRunDetail(null);
      return;
    }
    if (!selectedRunId) {
      const firstRealRun = runs.find((run) => isServerRunId(run.id));
      if (!firstRealRun) return;
      void openRunDetail(firstRealRun.id);
    }
  }, [openRunDetail, runs, selectedRunId]);

  const saveDisabled = saving || runningNow || !isValidNumericForm(form);

  const configListEmpty = !loadingConfigs && configs.length === 0;
  const runListEmpty = !loadingRuns && runs.length === 0;
  const selectedRunIsActive =
    selectedRunDetail?.run.status === "queued" || selectedRunDetail?.run.status === "running";

  useEffect(() => {
    if (!selectedRunDetail) return;
    if (selectedRunIsActive && runDetailTab === "result") {
      setRunDetailTab("conversation");
    }
  }, [runDetailTab, selectedRunDetail, selectedRunIsActive]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {error && (
        <Card>
          <CardContent className="pt-4">
            <p className="text-sm text-muted-foreground">{error}</p>
          </CardContent>
        </Card>
      )}

      {activeTab === "configs" && (
        <div className="min-h-0 flex-1">
          {loadingConfigs && configs.length === 0 ? (
            <div className="flex h-full min-h-0 items-center justify-center">
              <Spinner className="size-6 text-muted-foreground" />
            </div>
          ) : configListEmpty && !creatingConfig ? (
            <div className="flex h-full min-h-0 flex-col items-center justify-center gap-3 px-6 text-center">
              <h3 className="text-sm font-semibold">No Configurations Yet</h3>
              <p className="text-xs text-muted-foreground">
                Create your first Auto Test configuration to get started.
              </p>
              <Button onClick={startCreateConfig}>
                <HugeiconsIcon icon={Add01Icon} className="size-4" /> Add Configuration
              </Button>
            </div>
          ) : (
            <div
              className={`grid h-full min-h-0 ${configListHidden ? "lg:grid-cols-1" : "lg:grid-cols-[280px_minmax(0,1fr)]"}`}
            >
              {!configListHidden ? (
                <div className="relative min-h-0 border-r border-border">
                  <div className="flex min-h-0 h-full flex-col">
                    <div className="px-4 py-4">
                      <div className="flex items-center justify-between">
                        <h3 className="text-sm font-semibold">Configurations</h3>
                        <Button
                          size="sm"
                          variant="ghost"
                          className="text-primary hover:text-primary text-xs h-8 hover:bg-primary/10"
                          onClick={startCreateConfig}
                        >
                          <HugeiconsIcon icon={Add01Icon} className="size-3" /> Add
                        </Button>
                      </div>
                      <p className="text-xs text-muted-foreground">Saved templates</p>
                    </div>
                    <div className="min-h-0 flex-1 space-y-3 overflow-auto px-4 pt-1 pb-4">
                      {loadingConfigs ? <Spinner className="size-4 text-muted-foreground" /> : null}
                      {configs.map((config) => (
                        <button
                          key={config.id}
                          type="button"
                          onClick={() => void selectConfig(config.id)}
                          className={`w-full rounded-md ring-1 px-3 py-2 text-left transition-all ${
                            selectedConfigId === config.id && !creatingConfig
                              ? "opacity-100 ring-primary/20 shadow-sm"
                              : "opacity-75 bg-muted/50 ring-foreground/10 hover:bg-accent/30 hover:opacity-100"
                          }`}
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="text-xs font-medium">{config.name}</span>
                            <Badge variant="outline" className="text-[10px] font-mono">
                              v{config.latest_version}
                            </Badge>
                          </div>
                          <div className="mt-1 text-[10px] text-muted-foreground">
                            {formatDate(config.updated_at)}
                          </div>
                        </button>
                      ))}
                    </div>
                  </div>
                  <Button
                    type="button"
                    size="icon"
                    variant="outline"
                    className="absolute right-[-12px] top-1/2 z-10 size-6 -translate-y-1/2 rounded-full bg-background"
                    onClick={() => setConfigListHidden(true)}
                  >
                    <HugeiconsIcon icon={ArrowLeft01Icon} className="size-3.5" />
                  </Button>
                </div>
              ) : null}

              <div className="flex min-h-0 flex-col">
                <div className="px-4 py-4">
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2">
                      {configListHidden ? (
                        <Button
                          type="button"
                          variant="outline"
                          size="icon"
                          className="size-7"
                          onClick={() => setConfigListHidden(false)}
                        >
                          <HugeiconsIcon icon={PanelLeftIcon} className="size-4" />
                        </Button>
                      ) : null}
                      <CardTitle className="text-sm">
                        {creatingConfig ? "New Configuration" : "Configuration Detail"}
                      </CardTitle>
                    </div>
                    {!creatingConfig && selectedConfigId ? (
                      <div className="flex items-center gap-2">
                        <Button
                          type="button"
                          size="sm"
                          variant="ghost"
                          className="text-primary text-xs hover:text-primary hover:bg-primary/10"
                          disabled={
                            Boolean(runningConfigId) || runningNow || !isValidNumericForm(form)
                          }
                          onClick={() => {
                            if (isConfigFormDirty) {
                              void saveConfig(true);
                              return;
                            }
                            const selected = configs.find((item) => item.id === selectedConfigId);
                            if (selected) {
                              const targetVersion = loadedConfigVersion ?? selected.latest_version;
                              void runFromConfig(selected, targetVersion);
                            }
                          }}
                        >
                          {runningNow || runningConfigId === selectedConfigId ? (
                            <Spinner className="size-3.5" />
                          ) : (
                            <HugeiconsIcon icon={PlayIcon} className="size-3.5" />
                          )}
                          {runPrimaryLabel}
                        </Button>
                        <Button
                          type="button"
                          size="sm"
                          variant="ghost"
                          onClick={() => void saveConfig(false)}
                          className="text-xs"
                          disabled={!canSaveVersion}
                        >
                          {saving ? (
                            <Spinner className="size-3.5" />
                          ) : (
                            <HugeiconsIcon icon={GitBranchIcon} className="size-3.5" />
                          )}
                          Save
                        </Button>
                      </div>
                    ) : null}
                  </div>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {creatingConfig
                      ? "Create and save a reusable Auto Test setup."
                      : selectedConfigDetail
                        ? `${selectedConfigDetail.config.name} (Version ${
                            loadedConfigVersion ?? selectedConfigDetail.config.latest_version
                          }${isConfigFormDirty ? "*" : ""})`
                        : "Select a configuration from the list."}
                  </p>
                </div>
                <div className="min-h-0 flex-1 space-y-4 overflow-auto px-4 pb-4">
                  <div className="grid gap-2 md:grid-cols-2">
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">
                        Configuration Name
                      </label>
                      <Input
                        value={form.name}
                        onChange={(event) =>
                          setForm((previous) => ({
                            ...previous,
                            name: event.target.value,
                          }))
                        }
                        disabled={!creatingConfig}
                        placeholder="Auto test baseline"
                      />
                    </div>
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">Persona Preset</label>
                      <Select
                        value={form.persona_preset}
                        onValueChange={(value: string) =>
                          setForm((previous) => ({
                            ...previous,
                            persona_preset: value as PersonaPreset,
                          }))
                        }
                      >
                        <SelectTrigger>
                          <SelectValue placeholder="Select persona preset" />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="cooperative">
                            <SelectItemText>Cooperative</SelectItemText>
                          </SelectItem>
                          <SelectItem value="confused">
                            <SelectItemText>Confused</SelectItemText>
                          </SelectItem>
                          <SelectItem value="impatient">
                            <SelectItemText>Impatient</SelectItemText>
                          </SelectItem>
                          <SelectItem value="adversarial">
                            <SelectItemText>Adversarial</SelectItemText>
                          </SelectItem>
                          <SelectItem value="silent">
                            <SelectItemText>Silent</SelectItemText>
                          </SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                  </div>

                  <div className="space-y-2">
                    <label className="text-[10px] text-muted-foreground">
                      Persona Instructions
                    </label>
                    <Textarea
                      value={form.persona_instructions}
                      onChange={(event) =>
                        setForm((previous) => ({
                          ...previous,
                          persona_instructions: event.target.value,
                        }))
                      }
                      placeholder="Optional additional behavior guidance"
                    />
                  </div>

                  <div className="grid gap-2 md:grid-cols-2">
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">Scenario Profile</label>
                      <Select
                        value={form.scenario_profile}
                        onValueChange={(value: string) =>
                          setForm((previous) => ({
                            ...previous,
                            scenario_profile: value as ScenarioProfile,
                          }))
                        }
                      >
                        <SelectTrigger>
                          <SelectValue placeholder="Select scenario profile" />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="balanced">
                            <SelectItemText>Balanced</SelectItemText>
                          </SelectItem>
                          <SelectItem value="happy_path">
                            <SelectItemText>Happy Path</SelectItemText>
                          </SelectItem>
                          <SelectItem value="failure_heavy">
                            <SelectItemText>Failure Heavy</SelectItemText>
                          </SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">Evaluation Rubric</label>
                      <Select
                        value={form.rubric_version}
                        onValueChange={(value: string) =>
                          setForm((previous) => ({
                            ...previous,
                            rubric_version: value,
                          }))
                        }
                        disabled={loadingRubrics}
                      >
                        <SelectTrigger>
                          <SelectValue
                            placeholder={
                              loadingRubrics ? "Loading rubrics..." : "Select rubric preset"
                            }
                          />
                        </SelectTrigger>
                        <SelectContent>
                          {rubrics.map((rubric) => (
                            <SelectItem key={rubric.id} value={rubric.id}>
                              <div className="flex flex-col gap-0.5">
                                <SelectItemText>{rubric.display_name}</SelectItemText>
                                <span className="text-xs text-muted-foreground">
                                  {rubric.dimensions.map((d) => d.label).join(", ")}
                                </span>
                              </div>
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  </div>

                  <div className="grid gap-2 md:grid-cols-3">
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">Max Turns</label>
                      <Input
                        value={form.max_turns}
                        onChange={(event) =>
                          setForm((previous) => ({
                            ...previous,
                            max_turns: event.target.value,
                          }))
                        }
                      />
                    </div>
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">Timeout (s)</label>
                      <Input
                        value={form.timeout_seconds}
                        onChange={(event) =>
                          setForm((previous) => ({
                            ...previous,
                            timeout_seconds: event.target.value,
                          }))
                        }
                      />
                    </div>
                    <div className="space-y-2">
                      <label className="text-[10px] text-muted-foreground">Seed</label>
                      <Input
                        value={form.seed}
                        onChange={(event) =>
                          setForm((previous) => ({
                            ...previous,
                            seed: event.target.value,
                          }))
                        }
                      />
                    </div>
                  </div>

                  <div className="space-y-3">
                    <div className="flex items-center justify-between">
                      <div className="text-sm text-muted-foreground">Goals</div>
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={addGoal}
                        className="text-xs"
                      >
                        <HugeiconsIcon icon={Add01Icon} className="size-3.5" /> Add Goal
                      </Button>
                    </div>
                    {form.goals.length === 0 ? (
                      <p className="text-xs text-muted-foreground">No goals configured yet.</p>
                    ) : null}
                    {form.goals.map((goal) => (
                      <Card key={goal.localId}>
                        <CardContent className="space-y-2 pt-4">
                          <div className="grid gap-2 md:grid-cols-2">
                            <Input
                              value={goal.id}
                              onChange={(event) =>
                                updateGoal(goal.localId, "id", event.target.value)
                              }
                              placeholder="Goal ID"
                            />
                            <Input
                              value={goal.title}
                              onChange={(event) =>
                                updateGoal(goal.localId, "title", event.target.value)
                              }
                              placeholder="Goal title"
                            />
                          </div>
                          <Textarea
                            value={goal.description}
                            onChange={(event) =>
                              updateGoal(goal.localId, "description", event.target.value)
                            }
                            placeholder="Goal description"
                          />
                          <Textarea
                            value={goal.success_criteria}
                            onChange={(event) =>
                              updateGoal(goal.localId, "success_criteria", event.target.value)
                            }
                            placeholder="Success criteria"
                          />
                          <div className="flex justify-end">
                            <Button
                              type="button"
                              variant="ghost"
                              size="sm"
                              onClick={() => removeGoal(goal.localId)}
                            >
                              <HugeiconsIcon icon={Cancel01Icon} className="size-3.5" /> Remove
                            </Button>
                          </div>
                        </CardContent>
                      </Card>
                    ))}
                  </div>

                  <div className="flex items-center py-4 gap-2">
                    <input
                      id="judge-enabled"
                      type="checkbox"
                      checked={form.judge_enabled}
                      onChange={(event) =>
                        setForm((previous) => ({
                          ...previous,
                          judge_enabled: event.target.checked,
                        }))
                      }
                      className="h-4 w-4 rounded border border-border"
                    />
                    <label htmlFor="judge-enabled" className="text-xs text-muted-foreground">
                      Enable LLM judge
                    </label>
                  </div>

                  {!creatingConfig && selectedConfigDetail ? (
                    <>
                      <Separator />
                      <div className="space-y-2">
                        <div className="text-sm font-medium">Versions</div>
                        <TooltipProvider delayDuration={150}>
                          {selectedConfigDetail.versions
                            .slice()
                            .sort((a, b) => b.version - a.version)
                            .map((version) => {
                              const isLoadedVersion = loadedConfigVersion === version.version;
                              const showDiscardForLoadedDirty =
                                isLoadedVersion && isConfigFormDirty;

                              return (
                                <div
                                  key={version.id}
                                  className={`flex items-center justify-between rounded-md ring-1 px-3 py-2 ${
                                    isLoadedVersion
                                      ? "ring-primary/20 bg-primary/5"
                                      : "ring-foreground/10"
                                  }`}
                                >
                                  <div>
                                    <div className="text-xs font-medium">
                                      Version {version.version}
                                    </div>
                                    <div className="text-[10px] text-muted-foreground">
                                      {formatDate(version.created_at)}
                                    </div>
                                  </div>
                                  <div className="flex items-center gap-1">
                                    <Tooltip>
                                      <TooltipTrigger asChild>
                                        <Button
                                          type="button"
                                          size="icon"
                                          variant="ghost"
                                          className={`size-7 hover:bg-primary/10 ${isLoadedVersion ? "text-primary hover:text-primary" : ""}`}
                                          disabled={Boolean(runningConfigId) || runningNow}
                                          onClick={() => {
                                            const selected = configs.find(
                                              (item) => item.id === selectedConfigId
                                            );
                                            if (selected) {
                                              void runFromConfig(selected, version.version);
                                            }
                                          }}
                                        >
                                          <HugeiconsIcon icon={PlayIcon} className="size-3.5" />
                                        </Button>
                                      </TooltipTrigger>
                                      <TooltipContent>Run this version</TooltipContent>
                                    </Tooltip>
                                    <Tooltip>
                                      <TooltipTrigger asChild>
                                        <Button
                                          type="button"
                                          size="icon"
                                          variant="ghost"
                                          className={`size-7 hover:bg-primary/10 ${isLoadedVersion ? "text-primary hover:text-primary" : ""}`}
                                          onClick={() => {
                                            setLoadedConfigVersion(version.version);
                                            setForm(
                                              formFromPayload(
                                                selectedConfigDetail.config.name,
                                                version.config
                                              )
                                            );
                                          }}
                                        >
                                          {showDiscardForLoadedDirty ? (
                                            <HugeiconsIcon icon={UndoIcon} className="size-3.5" />
                                          ) : (
                                            <HugeiconsIcon
                                              icon={HardDriveUploadIcon}
                                              className="size-3.5"
                                            />
                                          )}
                                        </Button>
                                      </TooltipTrigger>
                                      <TooltipContent>
                                        {showDiscardForLoadedDirty
                                          ? "Discard changes"
                                          : "Load to form"}
                                      </TooltipContent>
                                    </Tooltip>
                                  </div>
                                </div>
                              );
                            })}
                        </TooltipProvider>
                      </div>
                    </>
                  ) : null}

                  {creatingConfig ? (
                    <>
                      <Separator />
                      <div className="flex flex-wrap gap-2">
                        <Button
                          variant="default"
                          onClick={() => void saveConfig(false)}
                          disabled={saveDisabled}
                        >
                          {saving ? <Spinner className="size-4" /> : null}
                          Save Config
                        </Button>
                        <Button
                          variant="outline"
                          onClick={() => void saveConfig(true)}
                          disabled={saveDisabled}
                        >
                          {runningNow ? (
                            <Spinner className="size-4" />
                          ) : (
                            <HugeiconsIcon icon={PlayIcon} className="size-4" />
                          )}
                          Save + Run Now
                        </Button>
                      </div>
                    </>
                  ) : null}
                </div>
              </div>
            </div>
          )}
        </div>
      )}

      {runStartedToast ? (
        <div className="pointer-events-none fixed bottom-5 right-5 z-50">
          <div className="rounded-md border border-border bg-card px-3 py-2 text-sm text-foreground shadow-sm">
            {runStartedToast}
          </div>
        </div>
      ) : null}

      {activeTab === "runs" && (
        <div className="min-h-0 flex-1">
          {loadingRuns && runs.length === 0 ? (
            <div className="flex h-full min-h-0 items-center justify-center">
              <Spinner className="size-6 text-muted-foreground" />
            </div>
          ) : runListEmpty ? (
            <div className="flex h-full min-h-0 flex-col items-center justify-center gap-2 px-6 text-center">
              <h3 className="text-sm font-semibold">No Run History Yet</h3>
              <p className="text-xs text-muted-foreground">
                Run an Auto Test configuration to generate history.
              </p>
            </div>
          ) : (
            <div
              className={`grid h-full min-h-0 ${runListHidden ? "lg:grid-cols-1" : "lg:grid-cols-[280px_minmax(0,1fr)]"}`}
            >
              {!runListHidden ? (
                <div className="relative min-h-0 border-r border-border">
                  <div className="flex min-h-0 h-full flex-col">
                    <div className="px-4 py-4">
                      <div className="flex items-center justify-between gap-2">
                        <div className="flex items-center gap-2">
                          <h3 className="text-sm font-semibold">History</h3>
                          {loadingRuns && runs.length > 0 ? (
                            <Spinner className="size-4 text-muted-foreground" />
                          ) : null}
                        </div>
                        <AlertDialog open={clearDialogOpen} onOpenChange={setClearDialogOpen}>
                          <AlertDialogTrigger asChild>
                            <Button
                              size="sm"
                              variant="ghost"
                              disabled={loadingRuns || runs.length === 0}
                              className="text-xs"
                            >
                              <HugeiconsIcon icon={Delete02Icon} className="size-3.5" /> Clear
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>Clear run history?</AlertDialogTitle>
                              <AlertDialogDescription>
                                This removes all finished runs. Active runs will be kept.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Cancel</AlertDialogCancel>
                              <AlertDialogAction onClick={() => void clearHistory()}>
                                Clear
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      </div>
                      <p className="text-xs text-muted-foreground">Recent and live test runs</p>
                    </div>
                    <div className="min-h-0 flex-1 space-y-3 overflow-auto px-4 pt-1 pb-4">
                      {loadingRuns && runs.length === 0 ? (
                        <Spinner className="size-4 text-muted-foreground" />
                      ) : null}
                      {runs.map((run) => (
                        <button
                          key={run.id}
                          type="button"
                          onClick={() => void openRunDetail(run.id)}
                          className={`w-full rounded-md ring-1 px-3 py-2.5 text-left transition-all ${
                            selectedRunId === run.id
                              ? "ring-primary/20 opacity-100 shadow-sm"
                              : "ring-foreground/10 opacity-75 bg-muted/50 hover:bg-accent/30 hover:opacity-100"
                          }`}
                        >
                          <div className="flex items-center justify-between gap-2">
                            <span className="truncate text-xs font-medium">
                              {configNameById[run.config_id] ?? "Unknown Configuration"} (v
                              {run.config_version})
                            </span>
                            <Badge variant="secondary" className="text-[10px] font-mono">
                              {run.aggregate_score != null ? `${run.aggregate_score}` : "-"}
                            </Badge>
                          </div>
                          <div className="mt-1 flex items-center justify-between gap-2">
                            <span
                              className={`text-[10px] font-medium ${statusTextClass(run.status)}`}
                            >
                              {humanizeLabel(run.status)}
                            </span>
                            <span className="text-[10px] text-muted-foreground">
                              {formatDate(run.created_at)}
                            </span>
                          </div>
                        </button>
                      ))}
                    </div>
                  </div>
                  <Button
                    type="button"
                    size="icon"
                    variant="outline"
                    className="absolute right-[-12px] top-1/2 z-10 size-6 -translate-y-1/2 rounded-full bg-background"
                    onClick={() => setRunListHidden(true)}
                  >
                    <HugeiconsIcon icon={ArrowLeft01Icon} className="size-3.5" />
                  </Button>
                </div>
              ) : null}

              <div className="flex min-h-0 flex-col">
                <div className="px-4 py-4">
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2">
                      {runListHidden ? (
                        <Button
                          type="button"
                          variant="outline"
                          size="icon"
                          className="size-7"
                          onClick={() => setRunListHidden(false)}
                        >
                          <HugeiconsIcon icon={PanelLeftIcon} className="size-4" />
                        </Button>
                      ) : null}
                      <CardTitle className="text-sm">Test Run Detail</CardTitle>
                      {loadingRunDetail ? (
                        <Spinner className="size-4 text-muted-foreground" />
                      ) : null}
                    </div>
                    {selectedRunDetail ? (
                      <div className="flex items-center gap-2">
                        {(selectedRunDetail.run.status === "queued" ||
                          selectedRunDetail.run.status === "running") && (
                          <Button
                            size="sm"
                            variant="ghost"
                            className="text-xs"
                            disabled={Boolean(runActionLoading[selectedRunDetail.run.id])}
                            onClick={() => void cancelRun(selectedRunDetail.run)}
                          >
                            {runActionLoading[selectedRunDetail.run.id] ? (
                              <Spinner className="size-3.5" />
                            ) : (
                              <HugeiconsIcon icon={SquareIcon} className="size-3.5" />
                            )}
                            Cancel
                          </Button>
                        )}
                        <Button
                          size="sm"
                          variant="ghost"
                          className="text-xs"
                          disabled={
                            Boolean(runActionLoading[selectedRunDetail.run.id]) ||
                            selectedRunIsActive
                          }
                          onClick={() => void rerun(selectedRunDetail.run)}
                        >
                          {runActionLoading[selectedRunDetail.run.id] ? (
                            <Spinner className="size-3.5" />
                          ) : (
                            <HugeiconsIcon icon={RedoIcon} className="size-3.5" />
                          )}
                          Rerun
                        </Button>
                        <AlertDialog open={removeDialogOpen} onOpenChange={setRemoveDialogOpen}>
                          <AlertDialogTrigger asChild>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="text-xs"
                              disabled={
                                Boolean(runActionLoading[selectedRunDetail.run.id]) ||
                                selectedRunDetail.run.status === "queued" ||
                                selectedRunDetail.run.status === "running"
                              }
                            >
                              {runActionLoading[selectedRunDetail.run.id] ? (
                                <Spinner className="size-3.5" />
                              ) : (
                                <HugeiconsIcon icon={Delete02Icon} className="size-3.5" />
                              )}
                              Remove
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>Remove this run?</AlertDialogTitle>
                              <AlertDialogDescription>
                                This removes the selected run history item and its timeline. This
                                cannot be undone.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Cancel</AlertDialogCancel>
                              <AlertDialogAction
                                onClick={() => void removeRun(selectedRunDetail.run)}
                              >
                                Remove
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      </div>
                    ) : null}
                  </div>
                </div>
                <div className="min-h-0 flex-1 space-y-6 overflow-auto px-4 pb-4">
                  {selectedRunDetail ? (
                    <>
                      <div className="grid gap-3 pb-4 md:grid-cols-3">
                        <div className="rounded-md border border-border p-3">
                          <div className="text-[10px] text-muted-foreground mb-1">Status</div>
                          <div
                            className={`text-sm font-semibold ${statusTextClass(selectedRunDetail.run.status)}`}
                          >
                            {selectedRunDetail.run.status}
                          </div>
                        </div>
                        <div className="rounded-md border border-border p-3">
                          <div className="text-[10px] text-muted-foreground mb-1">
                            Aggregate Score
                          </div>
                          <div className="text-sm font-semibold">
                            {formatAggregateScore(selectedRunDetail.run.aggregate_score)}
                          </div>
                        </div>
                        <div className="rounded-md border border-border p-3">
                          <div className="text-[10px] text-muted-foreground mb-1">
                            Configuration
                          </div>
                          <div className="text-sm font-semibold">
                            {configNameById[selectedRunDetail.run.config_id] ??
                              "Unknown Configuration"}{" "}
                            · v{selectedRunDetail.run.config_version}
                          </div>
                        </div>
                      </div>

                      <div className="sticky top-0 z-10 -mx-4 border-b border-border bg-card px-4">
                        <div className="flex items-end justify-between gap-3">
                          <div className="flex items-end gap-5">
                            <button
                              type="button"
                              className={`border-b-2 pb-2 text-xs px-2 ${
                                runDetailTab === "result"
                                  ? "border-primary font-semibold text-foreground"
                                  : "border-transparent text-muted-foreground"
                              } ${selectedRunIsActive ? "cursor-not-allowed opacity-50" : ""}`}
                              disabled={selectedRunIsActive}
                              onClick={() => setRunDetailTab("result")}
                            >
                              Result
                            </button>
                            <button
                              type="button"
                              className={`border-b-2 pb-2 text-xs px-2 ${
                                runDetailTab === "conversation"
                                  ? "border-primary font-semibold text-foreground"
                                  : "border-transparent text-muted-foreground"
                              }`}
                              onClick={() => setRunDetailTab("conversation")}
                            >
                              Conversation
                            </button>
                            <button
                              type="button"
                              className={`border-b-2 pb-2 text-xs px-2 ${
                                runDetailTab === "events"
                                  ? "border-primary font-semibold text-foreground"
                                  : "border-transparent text-muted-foreground"
                              }`}
                              onClick={() => setRunDetailTab("events")}
                            >
                              Events
                            </button>
                          </div>
                          {selectedRunIsActive ? (
                            <Spinner className="mb-2 size-4 text-muted-foreground" />
                          ) : null}
                        </div>
                      </div>

                      {runDetailTab === "result" ? (
                        <div className="@container space-y-6">
                          <div className="space-y-2">
                            <div className="text-xs font-semibold">Evaluation Summary</div>
                            <p className="text-xs text-muted-foreground">
                              {selectedRunDetail.summary ?? "No summary available."}
                            </p>
                          </div>

                          <div className="space-y-2">
                            <div className="text-xs font-semibold">Hard Checks</div>
                            {Object.keys(selectedRunDetail.hard_checks).length === 0 ? (
                              <p className="text-xs text-muted-foreground">No hard-check data.</p>
                            ) : (
                              <div className="grid grid-cols-1 gap-2 @lg:grid-cols-2">
                                {Object.entries(selectedRunDetail.hard_checks).map(
                                  ([key, value]) => (
                                    <div
                                      key={key}
                                      className="flex items-center justify-between rounded-md border border-border px-3 py-2 text-xs"
                                    >
                                      <div className="flex items-center gap-2">
                                        {value ? (
                                          <HugeiconsIcon
                                            icon={CheckmarkCircle02Icon}
                                            className="size-4 text-success"
                                          />
                                        ) : (
                                          <HugeiconsIcon
                                            icon={CancelCircleIcon}
                                            className="size-4 text-destructive"
                                          />
                                        )}
                                        <span>{humanizeLabel(key)}</span>
                                      </div>
                                      <span className="text-xs text-muted-foreground">
                                        {value ? "Passed" : "Failed"}
                                      </span>
                                    </div>
                                  )
                                )}
                              </div>
                            )}
                          </div>

                          <div className="space-y-2">
                            <div className="text-xs font-semibold">Rubric Scores</div>
                            {Object.keys(selectedRunDetail.rubric_scores).length === 0 ? (
                              <p className="text-xs text-muted-foreground">No rubric scores.</p>
                            ) : (
                              <div className="grid grid-cols-3 gap-2 @lg:grid-cols-5">
                                {Object.entries(selectedRunDetail.rubric_scores).map(
                                  ([key, value]) => (
                                    <div
                                      key={key}
                                      className="rounded-md border border-border px-3 py-3"
                                    >
                                      <div className="text-[10px] text-muted-foreground">
                                        {humanizeLabel(key)}
                                      </div>
                                      <div className="mt-1 text-lg font-semibold">{value}</div>
                                    </div>
                                  )
                                )}
                              </div>
                            )}
                          </div>
                        </div>
                      ) : null}

                      {runDetailTab === "conversation" ? (
                        <div className="space-y-2">
                          {selectedConversation.length === 0 ? (
                            <p className="text-sm text-muted-foreground">
                              No conversation events yet.
                            </p>
                          ) : (
                            <div className="space-y-2">
                              {selectedConversation.map((message) => (
                                <div
                                  key={`${message.seqNo}-${message.role}`}
                                  className={`rounded-md border px-3 py-2.5 text-xs ${
                                    message.role === "test_agent"
                                      ? "border-border bg-background text-foreground"
                                      : "border-primary/10 bg-primary/5 text-foreground"
                                  }`}
                                >
                                  <div
                                    className={`mb-2 text-[10px] font-mono tracking-wide ${
                                      message.role === "test_agent"
                                        ? "text-muted-foreground"
                                        : "text-primary/75"
                                    }`}
                                  >
                                    {message.role === "test_agent" ? "Test Agent" : "Target Agent"}{" "}
                                    · seq#
                                    {message.seqNo}
                                    {message.turnId !== null ? ` · turn ${message.turnId}` : ""}
                                  </div>
                                  <p className="whitespace-pre-wrap break-words">{message.text}</p>
                                </div>
                              ))}
                            </div>
                          )}
                        </div>
                      ) : null}

                      {runDetailTab === "events" ? (
                        <div className="space-y-3">
                          {selectedRunEvents.length === 0 ? (
                            <p className="text-sm text-muted-foreground">No events received yet.</p>
                          ) : (
                            <div className="space-y-2">
                              {selectedRunEvents.map((event) => (
                                <div
                                  key={event.seq_no}
                                  className="rounded-md border border-border px-3 py-2"
                                >
                                  <div className="flex items-center justify-between text-xs text-muted-foreground">
                                    <Badge variant="outline" className="text-[10px]">
                                      {event.event_type}
                                    </Badge>
                                    <span>#{event.seq_no}</span>
                                  </div>
                                  <pre className="mt-3 whitespace-pre-wrap break-words text-xs text-muted-foreground">
                                    {JSON.stringify(event, null, 2)}
                                  </pre>
                                </div>
                              ))}
                            </div>
                          )}
                        </div>
                      ) : null}
                    </>
                  ) : (
                    <p className="text-sm text-muted-foreground">
                      Select a run from the left list.
                    </p>
                  )}
                </div>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
