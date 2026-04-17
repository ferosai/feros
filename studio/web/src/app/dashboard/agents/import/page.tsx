"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  AiScanIcon,
  AudioWave01Icon,
  ArrowLeft01Icon,
  ArrowRight01Icon,
  ArrowRight02Icon,
  Cancel01Icon,
  CheckmarkCircle02Icon,
  CodeIcon,
  FileValidationIcon,
  OneCircleIcon,
  Robot01Icon,
  ThreeCircleIcon,
  TwoCircleIcon,
} from "@hugeicons/core-free-icons";
import Link from "next/link";
import { useCallback, useMemo, useState } from "react";
import { useRouter } from "next/navigation";

import { ImportConfigInput } from "@/components/agent/import-config-input";
import type { ImportConfigParseStatus } from "@/components/agent/import-config-input";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
import {
  ApiError,
  type AgentFullConfig,
  api,
  FULL_CONFIG_SCHEMA_URL,
  getErrorMessage,
  type AgentGraphConfig,
  type ImportIssue,
  type ImportValidationResponse,
} from "@/lib/api/client";

type Step = 1 | 2 | 3;

const MAPPABLE_PATHS = ["tts_provider", "tts_model", "voice_id"] as const;
const STEP_ITEMS = [
  { step: 1 as Step, label: "Config Input", icon: OneCircleIcon },
  { step: 2 as Step, label: "Validation & Resolution", icon: TwoCircleIcon },
  { step: 3 as Step, label: "Agent Metadata", icon: ThreeCircleIcon },
];

export default function ImportAgentPage() {
  const router = useRouter();

  const [step, setStep] = useState<Step>(1);
  const [rawConfig, setRawConfig] = useState("");
  const [inputMode, setInputMode] = useState<"file" | "manual">("file");
  const [fullConfig, setFullConfig] = useState<AgentFullConfig | null>(null);
  const [parsedConfig, setParsedConfig] = useState<AgentGraphConfig | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [parseStatus, setParseStatus] = useState<ImportConfigParseStatus>("empty");

  const [validating, setValidating] = useState(false);
  const [validation, setValidation] = useState<ImportValidationResponse | null>(null);
  const [mappings, setMappings] = useState<Record<string, string>>({});

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitIssueDetails, setSubmitIssueDetails] = useState<ImportIssue[]>([]);
  const [submitting, setSubmitting] = useState(false);

  const handleParsedChange = useCallback(
    (nextParsed: unknown, nextError: string | null, nextStatus: ImportConfigParseStatus) => {
      if (!nextParsed || nextError) {
        setFullConfig(null);
        setParsedConfig(null);
        setParseError(nextError);
      } else if (isAgentFullConfig(nextParsed)) {
        setFullConfig(nextParsed);
        setParsedConfig(nextParsed.config);
        setParseError(null);
      } else {
        setFullConfig(null);
        setParsedConfig(null);
        setParseError(
          "Import expects a full config export payload with $schema set to agent-config-v1."
        );
      }
      setParseStatus(nextStatus);
    },
    []
  );

  const allIssues = useMemo(() => {
    if (!validation) return [];
    return [...validation.schema_issues, ...validation.fulfillment_issues];
  }, [validation]);

  const blockingIssues = useMemo(
    () => allIssues.filter((issue) => issue.blocking && issue.severity === "error"),
    [allIssues]
  );
  const fulfillmentIssues = useMemo(() => validation?.fulfillment_issues ?? [], [validation]);
  const hasFulfillmentIssues = fulfillmentIssues.length > 0;

  const mappingRows = useMemo(
    () =>
      fulfillmentIssues
        .filter((issue) => issue.mappable && issue.path)
        .map((issue) => ({
          property: issue.path as string,
          fromValue: String(
            validation?.normalized_config?.[issue.path as keyof AgentGraphConfig] ?? ""
          ),
          mappedTo: mappings[issue.path as string] || issue.suggested_value || "",
        })),
    [fulfillmentIssues, mappings, validation]
  );

  const canResolveBlockingViaMappings = useMemo(
    () =>
      blockingIssues.length > 0 &&
      blockingIssues.every((issue) => {
        const path = issue.path || "";
        if (!issue.mappable || !path) return false;
        return Boolean(mappings[path]?.trim());
      }),
    [blockingIssues, mappings]
  );

  const canProceedFromStep2 = blockingIssues.length === 0 || canResolveBlockingViaMappings;

  const runValidation = async () => {
    if (!parsedConfig) return;
    setValidating(true);
    setSubmitError(null);
    setSubmitIssueDetails([]);
    try {
      const result = await api.agents.importValidate(parsedConfig);
      setValidation(result);
      if (fullConfig) {
        if (!name.trim() && fullConfig.name) setName(fullConfig.name);
        if (!description.trim() && fullConfig.description) {
          setDescription(fullConfig.description);
        }
      }

      const nextMappings: Record<string, string> = { ...result.suggested_mappings };
      for (const issue of result.fulfillment_issues) {
        if (issue.mappable && issue.path && issue.suggested_value) {
          nextMappings[issue.path] = nextMappings[issue.path] || issue.suggested_value;
        }
      }
      setMappings(nextMappings);
      setStep(2);
    } catch (error) {
      setSubmitError(getErrorMessage(error, "Validation failed"));
    } finally {
      setValidating(false);
    }
  };

  const handleSubmit = async () => {
    if (!validation || !name.trim()) return;
    setSubmitting(true);
    setSubmitError(null);
    setSubmitIssueDetails([]);
    try {
      const payloadMappings = Object.fromEntries(
        Object.entries(mappings).filter(
          ([path, value]) =>
            fulfillmentIssues.some((issue) => issue.mappable && issue.path === path) &&
            MAPPABLE_PATHS.includes(path as (typeof MAPPABLE_PATHS)[number]) &&
            Boolean(value?.trim())
        )
      );

      const agent = await api.agents.import({
        name: name.trim(),
        description: description.trim() || undefined,
        full_config: {
          $schema: fullConfig?.$schema ?? FULL_CONFIG_SCHEMA_URL,
          name: fullConfig?.name ?? name.trim(),
          description: fullConfig?.description ?? (description.trim() ? description.trim() : null),
          config: validation.normalized_config,
          mermaid_diagram: fullConfig?.mermaid_diagram ?? null,
          connections: fullConfig?.connections ?? [],
        },
        mapping_mode: hasFulfillmentIssues ? "map_defaults" : "strict",
        mappings: payloadMappings,
      });

      router.push(`/dashboard/agents/${agent.id}`);
    } catch (error) {
      setSubmitError(getErrorMessage(error, "Import failed"));
      if (error instanceof ApiError && error.detail && typeof error.detail === "object") {
        const detail = error.detail as { issues?: unknown };
        if (Array.isArray(detail.issues)) {
          const parsedIssues = detail.issues.filter((item): item is ImportIssue =>
            Boolean(item && typeof item === "object" && "message" in item && "code" in item)
          );
          setSubmitIssueDetails(parsedIssues);
        }
      }
      setSubmitting(false);
    }
  };

  const renderSchemaIssues = (issues: ImportIssue[]) => (
    <div className="space-y-3">
      <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">Schema</p>
      {issues.length === 0 ? (
        <div className="rounded-lg border border-success/20 bg-success/5 px-3 py-2 text-sm text-success">
          <span className="inline-flex items-center gap-1.5">
            <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-4" />
            Everything looks good.
          </span>
        </div>
      ) : (
        <div className="space-y-2">
          {issues.map((issue, index) => (
            <div
              key={`${issue.code}-${issue.path || index}`}
              className="rounded-lg border bg-muted/30 px-3 py-2"
            >
              <p className="text-sm font-medium">{issue.message}</p>
              <p className="mt-1 text-xs text-muted-foreground">
                {issue.code}
                {issue.path ? ` • ${issue.path}` : ""}
                {issue.mappable ? " • mappable" : ""}
              </p>
            </div>
          ))}
        </div>
      )}
    </div>
  );

  const renderFulfillmentSection = (issues: ImportIssue[]) => (
    <div className="space-y-3">
      <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Compatibility
      </p>
      {issues.length === 0 ? (
        <div className="rounded-lg border border-success/20 bg-success/5 px-3 py-2 text-sm text-success">
          <span className="inline-flex items-center gap-1.5">
            <HugeiconsIcon icon={CheckmarkCircle02Icon} className="size-4" />
            Everything looks good.
          </span>
        </div>
      ) : (
        <>
          <div className="space-y-2">
            {issues.map((issue, index) => (
              <div
                key={`${issue.code}-${issue.path || index}`}
                className="rounded-lg border bg-muted/30 px-3 py-2"
              >
                <p className="text-sm font-medium">{issue.message}</p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {issue.code}
                  {issue.path ? ` • ${issue.path}` : ""}
                  {issue.mappable ? " • mappable to default" : ""}
                </p>
              </div>
            ))}
          </div>

          {mappingRows.length > 0 && (
            <div className="rounded-lg border border-border/80 bg-muted/20 p-3">
              <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                Auto Fix
              </p>
              <ul className="mt-2 list-disc space-y-2 pl-4 text-xs">
                {mappingRows.map((row, index) => (
                  <li key={`${row.property}-${index}`} className="text-muted-foreground">
                    <span className="mr-3 font-bold text-foreground">{row.property}</span>
                    <span className="inline-flex items-center gap-1.5">
                      <span
                        className={`font-mono ${row.fromValue ? "text-foreground" : "text-destructive"}`}
                      >
                        {row.fromValue || "unknown"}
                      </span>
                      <HugeiconsIcon
                        icon={ArrowRight02Icon}
                        className="size-3.5 text-muted-foreground"
                      />
                      <span
                        className={`font-mono ${row.mappedTo ? "text-foreground" : "text-destructive"}`}
                      >
                        {row.mappedTo || "no default"}
                      </span>
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}
    </div>
  );

  return (
    <div className="p-10 space-y-10 relative">
      <Link href="/dashboard/agents" className="absolute right-4 top-4 block w-fit">
        <Button variant="ghost" size="icon" className="rounded-full text-muted-foreground">
          <HugeiconsIcon icon={Cancel01Icon} className="size-5" />
        </Button>
      </Link>

      <div className="mx-auto max-w-2xl animate-in fade-in slide-in-from-bottom-4 duration-700">
        <Card className="glass-card shadow-2xl overflow-hidden border-0 ring-1 ring-primary/20 p-0">
          <div className="bg-primary/5 p-8 relative overflow-clip border-b border-primary/10">
            <div className="size-16 rounded-2xl bg-primary/10 flex items-center justify-center text-primary mb-6">
              <HugeiconsIcon icon={Robot01Icon} className="size-8" />
            </div>
            <h3 className="text-2xl font-bold tracking-tighter">Import Voice Agent</h3>
            <p className="text-muted-foreground mt-1">
              Validate imported config, resolve issues, and finalize metadata.
            </p>
            <HugeiconsIcon
              icon={AudioWave01Icon}
              className="size-52 absolute text-primary/10 -right-8 -bottom-16"
            />
          </div>

          <CardContent className="space-y-6 p-8 pt-6">
            <div className="-mt-1 pb-2 grid grid-cols-1 gap-3 sm:grid-cols-3">
              {STEP_ITEMS.map((item) => (
                <div
                  key={item.step}
                  className={`flex items-center gap-2 border-b px-1 pb-2 pt-1 text-xs transition-colors ${
                    step === item.step
                      ? "border-primary/75 text-primary font-semibold"
                      : "border-border text-muted-foreground"
                  }`}
                >
                  <HugeiconsIcon icon={item.icon} className="size-4 shrink-0" />
                  <span>{item.label}</span>
                </div>
              ))}
            </div>

            {step === 1 && (
              <>
                <ImportConfigInput
                  rawValue={rawConfig}
                  mode={inputMode}
                  onRawValueChange={setRawConfig}
                  onParsedChange={handleParsedChange}
                />

                <div className="flex items-center justify-between">
                  <p
                    className={`text-xs ${
                      parseError
                        ? "text-destructive"
                        : parseStatus === "invalid"
                          ? "text-destructive"
                          : parseStatus === "valid"
                            ? "text-success"
                            : "text-muted-foreground"
                    }`}
                  >
                    {parseError ? parseError : null}
                    {parseStatus === "empty" && "Add a JSON config to continue."}
                    {parseStatus === "parsing" && "Parsing JSON..."}
                    {parseStatus === "valid" && !parseError && "Valid full config payload."}
                    {parseStatus === "invalid" && !parseError && "Invalid JSON"}
                  </p>
                  <div className="flex items-center gap-2">
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => setInputMode(inputMode === "manual" ? "file" : "manual")}
                      className="h-8"
                    >
                      <HugeiconsIcon icon={CodeIcon} />
                      {inputMode === "manual" ? "Use File Upload" : "Use Manual Input"}
                    </Button>
                    <Button
                      onClick={runValidation}
                      disabled={!parsedConfig || Boolean(parseError) || validating}
                      className="h-8"
                    >
                      {validating ? (
                        <Spinner className="size-4" />
                      ) : (
                        <HugeiconsIcon icon={FileValidationIcon} />
                      )}
                      Validate config
                    </Button>
                  </div>
                </div>
              </>
            )}

            {step === 2 && validation && (
              <>
                {renderSchemaIssues(validation.schema_issues)}
                {renderFulfillmentSection(validation.fulfillment_issues)}

                <div className="flex items-center justify-between">
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => setStep(1)}
                    className="gap-1.5"
                  >
                    <HugeiconsIcon icon={ArrowLeft01Icon} className="size-4" /> Back
                  </Button>
                  <Button
                    onClick={() => setStep(3)}
                    disabled={!canProceedFromStep2}
                    className="gap-1.5"
                  >
                    Continue <HugeiconsIcon icon={ArrowRight01Icon} className="size-4" />
                  </Button>
                </div>
              </>
            )}

            {step === 3 && validation && (
              <>
                <div className="space-y-3">
                  <label className="block text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    Agent name
                  </label>
                  <Input
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    placeholder="e.g. Imported Support Agent"
                    autoFocus
                  />
                </div>

                <div className="space-y-3">
                  <label className="block text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    Description (optional)
                  </label>
                  <Textarea
                    value={description}
                    onChange={(e) => setDescription(e.target.value)}
                    placeholder="Brief context for this imported agent"
                    className="min-h-[120px]"
                  />
                </div>

                <div className="flex items-center justify-between">
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => setStep(2)}
                    className="gap-1.5"
                  >
                    <HugeiconsIcon icon={ArrowLeft01Icon} className="size-4" /> Back
                  </Button>
                  <Button
                    onClick={handleSubmit}
                    disabled={!name.trim() || submitting}
                    className="gap-2"
                  >
                    {submitting ? (
                      <Spinner className="size-4" />
                    ) : (
                      <HugeiconsIcon icon={AiScanIcon} className="size-4" />
                    )}
                    Import agent
                  </Button>
                </div>
              </>
            )}

            {submitError && (
              <div className="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive space-y-2">
                <p>{submitError}</p>
                {submitIssueDetails.length > 0 && (
                  <ul className="list-disc space-y-1 pl-4 text-xs">
                    {submitIssueDetails.map((issue, index) => (
                      <li key={`${issue.code}-${issue.path || index}`}>
                        <span className="font-mono">{issue.code}</span>
                        {issue.path ? (
                          <span>
                            {" "}
                            • <span className="font-mono">{issue.path}</span>
                          </span>
                        ) : null}
                        <span> • {issue.message}</span>
                        {issue.suggested_value ? (
                          <span>
                            {" "}
                            • suggested: <span className="font-mono">{issue.suggested_value}</span>
                          </span>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}

function isAgentFullConfig(value: unknown): value is AgentFullConfig {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return (
    candidate.$schema === FULL_CONFIG_SCHEMA_URL &&
    typeof candidate.name === "string" &&
    "config" in candidate &&
    typeof candidate.config === "object" &&
    candidate.config !== null &&
    !Array.isArray(candidate.config)
  );
}
