import { HugeiconsIcon } from "@hugeicons/react";
import {
  Calendar01Icon,
  Call02Icon,
  Copy01Icon,
  HashtagIcon,
  InformationCircleIcon,
  Robot01Icon,
  Tick01Icon,
  Timer01Icon,
} from "@hugeicons/core-free-icons";
import Link from "next/link";
import type { ReactNode } from "react";
import { useState } from "react";
import { useParams } from "next/navigation";

import type { CallLog } from "@/lib/api/client";
import { formatDateTime, formatDuration } from "./types";

export function CallHeader({ call }: { call: CallLog }) {
  const { workspace_slug } = useParams<{ workspace_slug: string }>();
  const occurredAt = call.started_at ?? call.created_at;
  const [copied, setCopied] = useState(false);

  const handleCopyId = async () => {
    try {
      await navigator.clipboard.writeText(call.id);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch {
      // ignore clipboard failures in unsupported contexts
    }
  };

  return (
    <section className="flat-card p-5 space-y-4">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold text-foreground">Call Detail</h2>
          <p className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground">
            <span>
              Call ID: <span className="font-mono">{call.id}</span>
            </span>
            <button
              type="button"
              onClick={handleCopyId}
              className="inline-flex h-5 w-5 items-center justify-center rounded border border-border/50 bg-secondary/40 text-muted-foreground transition-colors hover:text-foreground"
              title={copied ? "Copied" : "Copy call ID"}
              aria-label={copied ? "Copied" : "Copy call ID"}
            >
              {copied ? (
                <HugeiconsIcon icon={Tick01Icon} className="size-3" />
              ) : (
                <HugeiconsIcon icon={Copy01Icon} className="size-3" />
              )}
            </button>
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Link
            href={`/dashboard/${workspace_slug}/agents/${call.agent_id}`}
            className="inline-flex max-w-60 items-center hover:text-primary gap-2 rounded-md bg-secondary px-3 py-2 text-xs text-foreground transition-colors hover:bg-primary/10"
            title={call.agent_name ?? "Unknown Agent"}
          >
            <HugeiconsIcon icon={Robot01Icon} className="size-3.5" />
            <span className="truncate">{call.agent_name ?? "Unknown Agent"}</span>
          </Link>
          <span className="inline-flex items-center gap-2 rounded-md bg-secondary px-3 py-2 text-xs text-foreground capitalize">
            <HugeiconsIcon icon={InformationCircleIcon} className="size-3.5" />
            {call.status}
          </span>
        </div>
      </div>

      <div className="grid gap-3 md:grid-cols-3">
        <InfoItem
          label="Occurred At"
          value={formatDateTime(occurredAt)}
          icon={<HugeiconsIcon icon={Calendar01Icon} className="size-3.5" />}
        />
        <InfoItem
          label="Duration"
          value={formatDuration(call.duration_seconds)}
          icon={<HugeiconsIcon icon={Timer01Icon} className="size-3.5" />}
        />
        <InfoItem
          label="Direction"
          value={call.direction}
          icon={<HugeiconsIcon icon={Call02Icon} className="size-3.5" />}
        />
        <InfoItem
          label="Caller"
          value={call.caller_number ?? "—"}
          mono
          icon={<HugeiconsIcon icon={HashtagIcon} className="size-3.5" />}
        />
        <InfoItem
          label="Callee"
          value={call.callee_number ?? "—"}
          mono
          icon={<HugeiconsIcon icon={HashtagIcon} className="size-3.5" />}
        />
        <InfoItem
          label="Created At"
          value={formatDateTime(call.created_at)}
          icon={<HugeiconsIcon icon={Calendar01Icon} className="size-3.5" />}
        />
      </div>
    </section>
  );
}

function InfoItem({
  label,
  value,
  icon,
  mono,
}: {
  label: string;
  value: string;
  icon: ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="rounded-lg border border-border/40 bg-secondary/30 px-3 py-2.5">
      <p className="flex items-center gap-1.5 text-[10px] text-muted-foreground">
        {icon}
        {label}
      </p>
      <p className={`mt-1 text-sm text-foreground ${mono ? "font-mono" : ""}`}>{value}</p>
    </div>
  );
}
