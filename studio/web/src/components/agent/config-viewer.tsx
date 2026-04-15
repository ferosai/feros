"use client";

import ShikiCodeBlock from "@/components/ui/shiki-code-block";

interface ConfigViewerProps {
  config: Record<string, unknown>;
}

export default function ConfigViewer({ config }: ConfigViewerProps) {
  return (
    <div className="rounded-2xl border border-border overflow-hidden bg-[#ffffff] shadow-[0_12px_40px_rgba(0,0,0,0.04)]">
      <ShikiCodeBlock
        code={JSON.stringify(config, null, 2)}
        lang="json"
        className="min-h-[220px]"
      />
    </div>
  );
}
