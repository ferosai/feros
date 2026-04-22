"use client";

import { PageHeader } from "@/components/ui/page-header";

export default function WorkspaceSettingsPage() {
  return (
    <div className="flex flex-col h-full space-y-8 animate-in fade-in slide-in-from-bottom-4 duration-500 ease-out">
      <PageHeader
        title="Workspace Settings"
        description="Manage your organizational workspaces and team boundaries."
        icon={Building03Icon}
      />

      <div className="hidden in-[.open-source]:block text-sm text-muted-foreground">
        Workspace management is not available in the open source version.
      </div>
    </div>
  );
}
