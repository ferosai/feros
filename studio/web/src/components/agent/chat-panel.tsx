"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  AiBrain01Icon,
  ArrowDown01Icon,
  ArrowTurnBackwardIcon,
  ArtificialIntelligence08Icon,
  AttachmentIcon,
  Cancel01Icon,
  CheckmarkCircle02Icon,
  ConnectIcon,
  Wrench01Icon,
  SquareIcon,
} from "@hugeicons/core-free-icons";
import { useState, useRef, useEffect } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import {
  api,
  type AgentGraphConfig,
  type ActionCard,
  type Credential,
  type StreamPart,
  type RenderedPartData,
} from "@/lib/api/client";

// ── Types ────────────────────────────────────────────────────────

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  /** Structured parts — built progressively during streaming, loaded from DB column on reload. */
  parts?: RenderedPartData[];
  config?: AgentGraphConfig | null;
  changeSummary?: string | null;
  actionCards?: ActionCard[];
  isStreaming?: boolean;
  progressEvents?: ProgressEvent[];
}

export interface ProgressEvent {
  step: string;
  status: string;
  message: string;
}

interface ChatPanelProps {
  agentId: string;
  messages: ChatMessage[];
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>;
  onBuildStart: () => void;
  onBuildFinish: () => void;
  onConfigUpdate: () => void | Promise<void>;
  onMermaidUpdate: (diagram: string) => void;
  credentials: Credential[];
  recentlySavedSkill: string | null;
  onOpenCredentialModal: (card: ActionCard) => void;
  onDiff?: (description: string) => void;
}

export default function ChatPanel({
  agentId,
  messages,
  setMessages,
  onBuildStart,
  onBuildFinish,
  onConfigUpdate,
  onMermaidUpdate,
  credentials,
  recentlySavedSkill,
  onOpenCredentialModal,
  onDiff,
}: ChatPanelProps) {
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [inputFocused, setInputFocused] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [uploadedFiles, setUploadedFiles] = useState<
    { id: string; name: string; totalLines: number }[]
  >([]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  // Abort controller for the active stream
  const abortControllerRef = useRef<AbortController | null>(null);

  // Auto-scroll
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [messages]);

  // Auto-resize textarea
  useEffect(() => {
    const textarea = textareaRef.current;
    if (textarea) {
      textarea.style.height = "auto";
      textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`;
    }
  }, [input]);

  const handleFileChange = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setUploading(true);
    try {
      const result = await api.builder.upload(agentId, file);
      setUploadedFiles((p) => [
        ...p,
        {
          id: result.file_id,
          name: result.filename,
          totalLines: result.total_lines,
        },
      ]);
    } catch {
      /* ignore */
    } finally {
      setUploading(false);
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  const sendMessage = async () => {
    if (!input.trim() || sending) return;
    const content = input.trim();

    // Build user message parts for local display: attachment parts + text part
    const parts: RenderedPartData[] = [];
    const apiAttachments: {
      file_id: string;
      filename: string;
      total_lines: number;
    }[] = [];
    if (uploadedFiles.length > 0) {
      for (const f of uploadedFiles) {
        parts.push({
          kind: "attachment",
          content: "",
          file_id: f.id,
          filename: f.name,
          total_lines: f.totalLines,
        });
        apiAttachments.push({
          file_id: f.id,
          filename: f.name,
          total_lines: f.totalLines,
        });
      }
    }
    parts.push({ kind: "text", content });

    const userMsg: ChatMessage = {
      id: `user-${Date.now()}`,
      role: "user",
      parts,
    };
    const streamMsgId = `stream-${Date.now()}`;
    const streamMsg: ChatMessage = {
      id: streamMsgId,
      role: "assistant",
      parts: [],
      isStreaming: true,
      progressEvents: [],
    };
    setMessages((p) => [...p, userMsg, streamMsg]);
    setInput("");
    setSending(true);
    if (textareaRef.current) textareaRef.current.style.height = "auto";

    // Clear uploaded files after capturing them
    const sentAttachments = apiAttachments.length > 0 ? apiAttachments : undefined;
    if (sentAttachments) setUploadedFiles([]);

    // AbortController for this stream
    const ac = new AbortController();
    abortControllerRef.current = ac;

    const buildUiState = { buildUiStarted: false };
    const ensureBuildStarted = () => {
      if (buildUiState.buildUiStarted) return;
      buildUiState.buildUiStarted = true;
      onBuildStart();
    };

    try {
      await api.builder
        .streamMessage(
          agentId,
          content,
          {
            onMermaidStart: () => {
              ensureBuildStarted();
            },
            onPart: (evt: StreamPart) => {
              ensureBuildStarted();
              setMessages((prev) =>
                prev.map((m) => {
                  if (m.id !== streamMsgId) return m;
                  const parts = [...(m.parts || [])];

                  if (evt.kind === "part_start" && evt.part_kind) {
                    parts.push({
                      kind: evt.part_kind,
                      content: "",
                      tool_name: evt.tool_name,
                      args: evt.args,
                    });
                  } else if (evt.kind === "part_delta") {
                    let targetIdx = -1;
                    if (evt.part_kind) {
                      for (let j = parts.length - 1; j >= 0; j--) {
                        if (parts[j].kind === evt.part_kind) {
                          targetIdx = j;
                          break;
                        }
                      }
                    } else {
                      targetIdx = parts.length - 1;
                    }
                    if (targetIdx === -1 && evt.part_kind) {
                      parts.push({
                        kind: evt.part_kind,
                        content: "",
                        tool_name: evt.tool_name,
                        args: "",
                      });
                      targetIdx = parts.length - 1;
                    }
                    if (targetIdx >= 0) {
                      const t = { ...parts[targetIdx] };
                      if (t.kind === "tool-call") {
                        t.args = (t.args || "") + (evt.content || "");
                        if (evt.tool_name) t.tool_name = (t.tool_name || "") + evt.tool_name;
                      } else {
                        t.content += evt.content || "";
                      }
                      parts[targetIdx] = t;
                    }
                  } else if (evt.kind === "tool_return") {
                    parts.push({
                      kind: "tool-return",
                      content: evt.content || "",
                      tool_name: evt.tool_name,
                    });
                  }
                  return { ...m, parts };
                })
              );
            },
            onConfig: (config: AgentGraphConfig) => {
              setMessages((p) => p.map((m) => (m.id === streamMsgId ? { ...m, config } : m)));
              void onConfigUpdate();
            },
            onActionCards: (cards: ActionCard[]) => {
              setMessages((p) =>
                p.map((m) => (m.id === streamMsgId ? { ...m, actionCards: cards } : m))
              );
            },
            onMermaid: (diagram) => {
              onMermaidUpdate(diagram);
            },
            onProgress: (data) => {
              setMessages((p) =>
                p.map((m) =>
                  m.id === streamMsgId
                    ? {
                        ...m,
                        progressEvents: [
                          ...(m.progressEvents || []),
                          {
                            step: data.step,
                            status: data.status,
                            message: data.message,
                          },
                        ],
                      }
                    : m
                )
              );
            },
            onDiff: (desc) => {
              onDiff?.(desc);
            },
            onDone: (data) => {
              setMessages((p) =>
                p.map((m) =>
                  m.id === streamMsgId
                    ? {
                        ...m,
                        isStreaming: false,
                        changeSummary: data.change_summary ?? undefined,
                      }
                    : m
                )
              );
              void onConfigUpdate();
              onBuildFinish();
            },
            onError: (e: string) => {
              setMessages((p) =>
                p.map((m) =>
                  m.id === streamMsgId
                    ? {
                        ...m,
                        isStreaming: false,
                        parts: [{ kind: "text" as const, content: e }],
                      }
                    : m
                )
              );
              onBuildFinish();
            },
          },
          sentAttachments,
          ac.signal
        )
        .catch((e) => {
          if (e.name !== "AbortError") console.error(e);
        });
    } catch {
      onBuildFinish();
    } finally {
      setSending(false);
      abortControllerRef.current = null;
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  };

  const canSend = input.trim().length > 0 && !sending;
  const handleStop = () => {
    abortControllerRef.current?.abort();
    abortControllerRef.current = null;
    // Mark the streaming message as done
    setMessages((p) => p.map((m) => (m.isStreaming ? { ...m, isStreaming: false } : m)));
    onBuildFinish();
    setSending(false);
  };

  return (
    <>
      {/* Messages */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto min-h-0 custom-scrollbar">
        <div className="mx-auto py-6 px-5 space-y-4">
          {messages.map((msg) => (
            <div key={msg.id} className="animate-in fade-in-0 slide-in-from-bottom-2 duration-300">
              {msg.role === "user" ? (
                <div className="flex justify-end">
                  <div className="max-w-[85%] bg-secondary text-foreground rounded-2xl rounded-br-sm px-4 py-2.5 overflow-x-auto custom-scrollbar flex flex-col">
                    {/* Attachment chips */}
                    {(msg.parts || []).filter((p) => p.kind === "attachment").length > 0 && (
                      <div className="flex flex-wrap gap-1.5 mb-2">
                        {(msg.parts || [])
                          .filter((p) => p.kind === "attachment")
                          .map((p, i) => (
                            <div
                              key={i}
                              className="flex items-center gap-1 bg-muted border border-border rounded-lg px-2 py-1 text-[10px] text-foreground/75"
                            >
                              <HugeiconsIcon
                                icon={AttachmentIcon}
                                className="size-2.5 text-muted-foreground"
                              />
                              <span className="max-w-40 truncate">{p.filename}</span>
                              <span className="text-muted-foreground/50">
                                {p.total_lines} lines
                              </span>
                            </div>
                          ))}
                      </div>
                    )}
                    <p className="text-sm leading-relaxed whitespace-pre-wrap break-normal">
                      {(msg.parts || [])
                        .filter((p) => p.kind === "text")
                        .map((p) => p.content)
                        .join("")}
                    </p>
                  </div>
                </div>
              ) : (
                <div className="flex gap-3">
                  <div className="size-8 rounded-lg bg-primary/10 flex items-center justify-center text-primary shrink-0">
                    <HugeiconsIcon icon={ArtificialIntelligence08Icon} className="size-4" />
                  </div>
                  <div className="flex-1 min-w-0 space-y-3">
                    {/* Progress events */}
                    {msg.progressEvents && msg.progressEvents.length > 0 && (
                      <div className="space-y-1">
                        {msg.progressEvents.map((p, i) => (
                          <div key={i} className="flex items-center gap-2 text-[10px] font-mono">
                            <span
                              className={`size-1.5 rounded-full ${p.status === "done" ? "bg-success" : p.status === "error" ? "bg-destructive" : "bg-yellow-500 animate-pulse"}`}
                            />
                            <span className="text-muted-foreground">Step {p.step}:</span>
                            <span className="text-muted-foreground">{p.message}</span>
                          </div>
                        ))}
                      </div>
                    )}
                    <div className="prose prose-sm prose-neutral dark:prose-invert max-w-none text-sm text-foreground/90 prose-p:break-normal prose-p:leading-relaxed prose-strong:text-foreground prose-p:mt-2 prose-pre:border prose-pre:border-border prose-pre:bg-muted prose-code:text-foreground/90 prose-headings:text-foreground prose-headings:text-sm overflow-x-auto custom-scrollbar">
                      {(() => {
                        const parts: RenderedPartData[] = msg.parts || [];

                        if (parts.length === 0 && msg.isStreaming) {
                          return (
                            <div className="flex items-center gap-2 text-muted-foreground text-sm font-medium h-6">
                              <Spinner className="size-3.5" />
                              <span className="animate-pulse">Agent is thinking...</span>
                            </div>
                          );
                        }

                        return (
                          <div className="space-y-3 [&_details]:mt-3 [&_summary]:cursor-pointer [&_summary]:text-[10px] [&_summary]:font-mono [&_summary]:text-foreground/75 [&_summary]:transition-colors [&_summary]:select-none [&_summary]:list-none [&_summary]:flex [&_summary]:items-center [&_summary]:gap-1.5 [&_summary]:hover:text-primary [&_summary::-webkit-details-marker]:hidden">
                            {parts.map((part, i) => {
                              if (part.kind === "thinking") {
                                if (!part.content && !msg.isStreaming) return null;
                                return (
                                  <details
                                    key={i}
                                    className="group"
                                    open={msg.isStreaming || undefined}
                                  >
                                    <summary>
                                      <HugeiconsIcon
                                        icon={ArrowDown01Icon}
                                        className="size-3 -rotate-90 transition-transform group-open:rotate-0"
                                      />
                                      <HugeiconsIcon icon={AiBrain01Icon} className="size-3" />
                                      Thinking...
                                      {msg.isStreaming && !part.content && (
                                        <Spinner className="size-2.5 ml-1 text-muted-foreground" />
                                      )}
                                    </summary>
                                    {part.content && (
                                      <div className="relative mt-1 ml-1.5 pl-3 text-muted-foreground text-xs leading-relaxed before:absolute before:left-0 before:-top-1 before:bottom-0 before:w-px before:bg-border">
                                        <ReactMarkdown remarkPlugins={[remarkGfm]}>
                                          {part.content}
                                        </ReactMarkdown>
                                      </div>
                                    )}
                                  </details>
                                );
                              }
                              if (part.kind === "tool-call") {
                                // A tool call is in-flight if the message is streaming AND
                                // there's no matching tool-return for this specific call.
                                // Count how many prior tool-calls share the same name to
                                // match the Nth call to the Nth return.
                                const callName = part.tool_name;
                                let callIndex = 0;
                                for (let j = 0; j < i; j++) {
                                  if (
                                    parts[j].kind === "tool-call" &&
                                    parts[j].tool_name === callName
                                  )
                                    callIndex++;
                                }
                                let returnCount = 0;
                                const hasReturn = parts.some((p, j) => {
                                  if (j <= i) return false;
                                  if (p.kind === "tool-return" && p.tool_name === callName) {
                                    if (returnCount === callIndex) return true;
                                    returnCount++;
                                  }
                                  return false;
                                });
                                const isInFlight = msg.isStreaming && !hasReturn;
                                const hasExpandableContent = Boolean(part.args && part.args.trim());
                                if (!hasExpandableContent) {
                                  return (
                                    <div
                                      key={i}
                                      className="text-[10px] font-mono text-foreground/75 flex items-center gap-1.5"
                                    >
                                      <span className="size-3 shrink-0" aria-hidden />
                                      {isInFlight ? (
                                        <Spinner className="size-3 text-muted-foreground" />
                                      ) : (
                                        <HugeiconsIcon icon={Wrench01Icon} className="size-3" />
                                      )}
                                      {part.tool_name ?? "tool"}()
                                      {isInFlight && (
                                        <span className="text-muted-foreground animate-pulse">
                                          running...
                                        </span>
                                      )}
                                    </div>
                                  );
                                }
                                return (
                                  <details key={i} className="group" open={isInFlight}>
                                    <summary>
                                      <HugeiconsIcon
                                        icon={ArrowDown01Icon}
                                        className="size-3 -rotate-90 transition-transform group-open:rotate-0"
                                      />
                                      {isInFlight ? (
                                        <Spinner className="size-3 text-muted-foreground" />
                                      ) : (
                                        <HugeiconsIcon icon={Wrench01Icon} className="size-3" />
                                      )}
                                      {part.tool_name ?? "tool"}()
                                      {isInFlight && (
                                        <span className="text-muted-foreground animate-pulse">
                                          running...
                                        </span>
                                      )}
                                    </summary>
                                    <pre className="relative mt-1 ml-1.5 pl-3 text-muted-foreground text-[10px] leading-relaxed overflow-x-auto max-h-[200px] overflow-y-auto custom-scrollbar before:absolute before:left-0 before:-top-1 before:bottom-0 before:w-px before:bg-border">
                                      {part.args}
                                    </pre>
                                  </details>
                                );
                              }
                              if (part.kind === "tool-return" && part.content) {
                                return (
                                  <details key={i} className="group">
                                    <summary>
                                      <HugeiconsIcon
                                        icon={ArrowDown01Icon}
                                        className="size-3 -rotate-90 transition-transform group-open:rotate-0"
                                      />
                                      <HugeiconsIcon
                                        icon={CheckmarkCircle02Icon}
                                        className="size-3"
                                      />
                                      {part.tool_name ?? "tool"} result
                                    </summary>
                                    <div className="relative mt-1 ml-1.5 pl-3 text-muted-foreground text-xs leading-relaxed overflow-x-auto max-h-[200px] overflow-y-auto custom-scrollbar before:absolute before:left-0 before:-top-1 before:bottom-0 before:w-px before:bg-border">
                                      <ReactMarkdown remarkPlugins={[remarkGfm]}>
                                        {part.content}
                                      </ReactMarkdown>
                                    </div>
                                  </details>
                                );
                              }
                              if (part.kind === "text") {
                                if (!part.content && !msg.isStreaming) return null;
                                return (
                                  <div key={i}>
                                    <ReactMarkdown remarkPlugins={[remarkGfm]}>
                                      {part.content}
                                    </ReactMarkdown>
                                    {msg.isStreaming && i === parts.length - 1 && (
                                      <span className="inline-block w-0.5 h-4 bg-primary animate-pulse ml-0.5 align-text-bottom" />
                                    )}
                                  </div>
                                );
                              }
                              return null;
                            })}
                          </div>
                        );
                      })()}
                    </div>
                    {msg.changeSummary && (
                      <div className="flex items-center gap-2 py-1.5 px-3 rounded-lg bg-accent/50 border border-border">
                        <HugeiconsIcon
                          icon={CheckmarkCircle02Icon}
                          className="size-3 text-muted-foreground shrink-0"
                        />
                        <span className="text-[10px] font-medium text-muted-foreground">
                          {msg.changeSummary}
                        </span>
                      </div>
                    )}
                    {msg.actionCards?.map((card, i) => (
                      <div
                        key={i}
                        className="flex gap-4 p-4 rounded-2xl border border-border bg-card hover:bg-card hover:border-primary/30 hover:shadow-sm transition-all group"
                      >
                        <div className="size-10 rounded-xl bg-primary/5 flex items-center justify-center text-primary shrink-0 group-hover:bg-primary/10 transition-colors mt-0.5">
                          <HugeiconsIcon icon={ConnectIcon} className="size-5" />
                        </div>
                        <div className="flex-1 min-w-0 flex flex-col gap-2.5">
                          <div className="min-w-0">
                            <h4 className="font-bold text-sm text-foreground truncate mb-1">
                              {card.title}
                            </h4>
                            <p className="text-xs text-muted-foreground leading-relaxed line-clamp-2 lg:line-clamp-3 pr-2">
                              {card.description}
                            </p>
                          </div>
                          <div className="flex items-center gap-3 pt-1">
                            <Button
                              size="sm"
                              onClick={() =>
                                recentlySavedSkill === card.skill
                                  ? undefined
                                  : onOpenCredentialModal(card)
                              }
                              className={`h-8 text-xs font-bold shrink-0 transition-all ${
                                recentlySavedSkill === card.skill
                                  ? "bg-success hover:bg-success text-success-foreground"
                                  : ""
                              }`}
                              disabled={recentlySavedSkill === card.skill}
                            >
                              {recentlySavedSkill === card.skill ? (
                                <>
                                  <HugeiconsIcon
                                    icon={CheckmarkCircle02Icon}
                                    className="size-3.5"
                                  />{" "}
                                  Saved
                                </>
                              ) : credentials.find((c) => c.provider === card.skill) ? (
                                "Update"
                              ) : (
                                "Connect"
                              )}
                            </Button>
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      </div>

      {/* Chat Input */}
      <div className="shrink-0 px-5 pb-4 pt-2">
        <div className="mx-auto">
          {uploadedFiles.length > 0 && (
            <div className="flex flex-wrap gap-1.5 mb-2 px-1">
              {uploadedFiles.map((f) => (
                <div
                  key={f.id}
                  className="flex items-center gap-1 bg-accent/60 border border-border rounded-lg px-2 py-1 text-[10px] text-foreground/75"
                >
                  <HugeiconsIcon icon={AttachmentIcon} className="size-2.5 text-muted-foreground" />
                  <span className="max-w-30 truncate">{f.name}</span>
                  <button
                    onClick={() => setUploadedFiles((p) => p.filter((x) => x.id !== f.id))}
                    className="ml-0.5 text-muted-foreground hover:text-destructive"
                  >
                    <HugeiconsIcon icon={Cancel01Icon} className="size-2.5" />
                  </button>
                </div>
              ))}
            </div>
          )}
          <div
            className={`relative flex w-full flex-col rounded-2xl ring-1 bg-card/50 p-2 transition-all shadow-lg ${
              inputFocused ? "shadow-primary/20 ring-primary/20" : "ring-foreground/10 shadow"
            }`}
          >
            <textarea
              ref={textareaRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              onFocus={() => setInputFocused(true)}
              onBlur={() => setInputFocused(false)}
              placeholder="Describe the agent you want to build..."
              className="w-full resize-none border-none bg-transparent px-3 py-2.5 text-sm text-foreground outline-none ring-0 transition-all placeholder:text-muted-foreground/50 focus:ring-0 min-h-[52px] max-h-[200px]"
              rows={1}
            />
            <div className="flex items-center justify-between px-1.5 pb-0.5 pt-0.5">
              <div className="flex items-center gap-1">
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".pdf,.txt,.md,.docx"
                  className="hidden"
                  onChange={handleFileChange}
                />
                <button
                  className="flex size-8 items-center justify-center rounded-lg text-muted-foreground hover:text-primary hover:bg-secondary transition-all"
                  onClick={() => fileInputRef.current?.click()}
                  disabled={uploading}
                  aria-label="Upload document"
                  title="Upload document (PDF, TXT, MD, DOCX)"
                >
                  {uploading ? (
                    <Spinner className="size-4" />
                  ) : (
                    <HugeiconsIcon icon={AttachmentIcon} className="size-4" />
                  )}
                </button>
                <span className="text-[10px] text-muted-foreground/50 px-1">
                  <kbd className="font-mono px-0.5 py-px uppercase border rounded">Enter</kbd> to
                  send
                </span>
                <span className="text-[10px] text-muted-foreground/50 px-1">
                  <kbd className="font-mono px-0.5 py-px uppercase border rounded">Shift</kbd>
                  <kbd className="ml-0.5 font-mono px-0.5 py-px uppercase border rounded">
                    Enter
                  </kbd>{" "}
                  for newlines
                </span>
              </div>
              <button
                className={`flex h-8 w-8 items-center justify-center rounded-lg transition-all ${
                  sending
                    ? "bg-primary text-primary-foreground hover:opacity-90 shadow-sm "
                    : canSend
                      ? "bg-primary text-primary-foreground hover:opacity-90 shadow-sm "
                      : "cursor-not-allowed bg-secondary text-muted-foreground/40"
                }`}
                onClick={sending ? handleStop : sendMessage}
                disabled={!canSend && !sending}
                aria-label={sending ? "Stop" : "Send message"}
              >
                {sending ? (
                  <HugeiconsIcon icon={SquareIcon} className="size-3 fill-current" />
                ) : (
                  <HugeiconsIcon icon={ArrowTurnBackwardIcon} className="size-4 stroke-[2.5]" />
                )}
              </button>
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
