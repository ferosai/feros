"use client";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ArrowRight01Icon, Building03Icon, Plus, PlusSignIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { PageHeader } from "@/components/ui/page-header";

export default function WorkspaceSettingsPage() {
  return (
    <div className="flex flex-col h-full space-y-8">
      <PageHeader
        title="Workspace Settings"
        description="Manage your organizational workspaces and team boundaries."
        icon={Building03Icon}
      />

      <div className="flex-1 max-w-4xl grid grid-cols-1 md:grid-cols-12 gap-8 items-start">
        
        {/* Workspaces List */}
        <div className="md:col-span-7 space-y-6">
          <div className="bg-card/50 backdrop-blur-xl ring-1 ring-border/75 shadow-lg rounded-2xl overflow-hidden transition-all duration-300">
            <div className="p-6 border-b border-border/50 bg-muted/20">
              <h2 className="text-lg font-semibold tracking-tight text-foreground flex items-center gap-2">
                <HugeiconsIcon icon={Building03Icon} className="w-5 h-5 text-primary" />
                Active Workspaces
              </h2>
              <p className="text-sm text-muted-foreground mt-1">Select a workspace to switch contexts.</p>
            </div>
            <div className="p-6">
              {isLoading ? (
                <div className="flex items-center justify-center py-8 text-muted-foreground animate-pulse">
                  Loading workspaces...
                </div>
              ) : !organization ? (
                <div className="flex flex-col items-center justify-center py-12 text-center text-destructive">
                  <div className="bg-destructive/10 p-4 rounded-full mb-4">
                    <HugeiconsIcon icon={Building03Icon} className="w-6 h-6 text-destructive" />
                  </div>
                  <p className="font-medium">Failed to load organization.</p>
                  <p className="text-sm opacity-80 mt-1">Please try refreshing the page.</p>
                </div>
              ) : workspaces.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-12 text-center text-muted-foreground">
                  <div className="bg-muted p-4 rounded-full mb-4">
                    <HugeiconsIcon icon={Building03Icon} className="w-6 h-6 opacity-50" />
                  </div>
                  <p>No workspaces found.</p>
                </div>
              ) : (
                <ul className="space-y-3">
                  {workspaces.map((ws) => {
                    const isActive = activeWorkspaceId === ws.id;
                    return (
                      <li
                        key={ws.id}
                        className={`group flex items-center justify-between p-4 rounded-xl transition-all duration-300 ring-1 ${
                          isActive 
                            ? "bg-primary/5 ring-primary/20 shadow-sm ring-1" 
                            : "bg-background ring-border/75 hover:border-primary/30 hover:shadow-md hover:-translate-y-0.5 cursor-pointer"
                        }`}
                        onClick={() => !isActive && setActiveWorkspaceId(ws.id)}
                      >
                        <div className="flex items-center gap-4">
                          <div>
                            <div className="font-semibold text-foreground">{ws.name}</div>
                            {isMultiOrg && (
                              <div className="text-xs text-muted-foreground mt-0.5">
                                {ws.org_name}
                              </div>
                            )}
                          </div>
                        </div>
                        {isActive ? (
                          <div className="px-3 py-1 bg-primary text-primary-foreground text-xs font-medium rounded-full shadow-sm">
                            Active
                          </div>
                        ) : (
                          <Button variant="ghost" size="sm" className="opacity-0 group-hover:opacity-100 transition-opacity -mr-2">
                            Switch <HugeiconsIcon icon={ArrowRight01Icon} className="w-4 h-4 ml-1" />
                          </Button>
                        )}
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </div>
        </div>

        {/* Create Workspace */}
        <div className="md:col-span-5 flex flex-col">
          <div className="flex-1 bg-linear-to-br from-card to-card/50 backdrop-blur-xl ring ring-border/75 shadow-lg rounded-2xl p-6 relative overflow-hidden">
            {/* Decorative background accent */}
            <div className="absolute top-0 right-0 -mr-8 -mt-8 w-32 h-32 bg-primary/10 rounded-full blur-3xl pointer-events-none"></div>
            
            <div className="mb-6 relative z-10">
              <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-primary/10 text-primary mb-4">
                <HugeiconsIcon icon={Plus} className="w-5 h-5" />
              </div>
              <h2 className="text-lg font-semibold tracking-tight text-foreground">Create New Workspace</h2>
              <p className="text-sm text-muted-foreground mt-1">
                Spin up a new environment for a different project, team, or client.
              </p>
            </div>
            
            <form onSubmit={handleCreate} className="space-y-4 relative z-10">
              {isMultiOrg && (
                <div className="space-y-1.5">
                  <label className="text-sm font-medium text-foreground">Organization</label>
                  <div className="flex flex-wrap gap-2">
                    {Array.from(new Set(workspaces.map(w => w.org_id))).map(orgId => {
                      const orgName = workspaces.find(w => w.org_id === orgId)?.org_name || orgId;
                      return (
                        <button
                          key={orgId}
                          type="button"
                          onClick={() => setSelectedOrgId(orgId)}
                          className={`px-3 py-1 rounded-full text-xs border transition-colors ${
                            (selectedOrgId || organization?.id) === orgId
                              ? "bg-primary text-primary-foreground border-primary"
                              : "bg-transparent text-muted-foreground border-border hover:border-foreground/40"
                          }`}
                        >
                          {orgName}
                        </button>
                      );
                    })}
                  </div>
                </div>
              )}
              <div className="space-y-2">
                <label className="text-sm font-medium text-foreground">Workspace Name</label>
                <Input
                  value={newWorkspaceName}
                  onChange={(e) => setNewWorkspaceName(e.target.value)}
                  placeholder="e.g. Acme Corp Production"
                  className="bg-background/50 border-border/50 focus-visible:ring-primary/50 transition-all h-11"
                  disabled={isCreating}
                />
              </div>
              <Button 
                type="submit" 
                className="w-full h-11 font-medium shadow-md transition-all hover:shadow-lg active:scale-[0.98]" 
                disabled={isCreating || !newWorkspaceName.trim() || !organization}
              >
                {isCreating ? (
                  <span className="flex items-center">
                    <svg className="animate-spin -ml-1 mr-3 h-5 w-5 text-white" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                      <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4"></circle>
                      <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                    </svg>
                    Creating...
                  </span>
                ) : (
                  <span className="flex items-center">
                    <HugeiconsIcon icon={PlusSignIcon} className="w-5 h-5 mr-2" />
                    Create Workspace
                  </span>
                )}
              </Button>
            </form>
          </div>
        </div>
        
      </div>
      {/* END-INTERNAL */}

      <div className="hidden in-[.open-source]:block text-sm text-muted-foreground">
        Workspace management is not available in the open source version.
      </div>
    </div>
  );
}
