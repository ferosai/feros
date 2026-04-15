"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  ArrowRight01Icon,
  MinusSignIcon,
  PencilEdit01Icon,
} from "@hugeicons/core-free-icons";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";

interface DiffItem {
  type: "added" | "removed" | "modified";
  section: string;
  detail: string;
}

interface ConfigDiffProps {
  description: string;
}

/**
 * Parses a diff description into structured items.
 * The description can be a plain string or semi-structured text.
 */
function parseDiffDescription(description: string): DiffItem[] {
  const items: DiffItem[] = [];
  const lines = description.split("\n").filter((l) => l.trim());

  for (const line of lines) {
    const lower = line.toLowerCase();
    let type: DiffItem["type"] = "modified";
    if (lower.includes("add") || lower.includes("new") || lower.includes("create")) {
      type = "added";
    } else if (lower.includes("remove") || lower.includes("delete")) {
      type = "removed";
    }

    // Try to extract section from common patterns
    let section = "config";
    if (lower.includes("scene")) section = "scenes";
    else if (lower.includes("tool")) section = "tools";
    else if (lower.includes("knowledge")) section = "knowledge";
    else if (lower.includes("rule")) section = "rules";
    else if (lower.includes("identity")) section = "identity";

    items.push({ type, section, detail: line.trim() });
  }

  // If no structured items were found, return the whole description as one item
  if (items.length === 0 && description.trim()) {
    items.push({
      type: "modified",
      section: "config",
      detail: description.trim(),
    });
  }

  return items;
}

const typeConfig = {
  added: {
    icon: Add01Icon,
    color: "text-green-600",
    bg: "bg-green-50 border-green-200",
    badge: "default" as const,
  },
  removed: {
    icon: MinusSignIcon,
    color: "text-red-600",
    bg: "bg-red-50 border-red-200",
    badge: "destructive" as const,
  },
  modified: {
    icon: PencilEdit01Icon,
    color: "text-blue-600",
    bg: "bg-blue-50 border-blue-200",
    badge: "secondary" as const,
  },
};

export default function ConfigDiff({ description }: ConfigDiffProps) {
  const items = parseDiffDescription(description);

  if (items.length === 0) return null;

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-1.5">
        <HugeiconsIcon icon={ArrowRight01Icon} className="size-3 text-primary" />
        <h4 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
          Changes
        </h4>
      </div>
      {items.map((item, i) => {
        const cfg = typeConfig[item.type];
        return (
          <Card key={i} className={`border ${cfg.bg}`}>
            <CardContent className="p-2.5 flex items-start gap-2">
              <HugeiconsIcon icon={cfg.icon} className={`size-3.5 shrink-0 mt-0.5 ${cfg.color}`} />
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-1.5 mb-0.5">
                  <Badge variant={cfg.badge} className="text-[10px] h-3.5 px-1">
                    {item.type}
                  </Badge>
                  <span className="text-[10px] text-muted-foreground font-mono">
                    {item.section}
                  </span>
                </div>
                <p className="text-[10px] text-foreground/80">{item.detail}</p>
              </div>
            </CardContent>
          </Card>
        );
      })}
    </div>
  );
}
