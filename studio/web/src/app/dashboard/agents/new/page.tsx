"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  ArrowRight01Icon,
  Robot01Icon,
  AiBrowserIcon,
  AudioWave01Icon,
  Cancel01Icon,
} from "@hugeicons/core-free-icons";
import { useState } from "react";
import { useRouter } from "next/navigation";
import { api } from "@/lib/api/client";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Input } from "@/components/ui/input";
import { Card, CardContent } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";
import Link from "next/link";

export default function NewAgentPage() {
  const router = useRouter();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [creating, setCreating] = useState(false);

  const handleCreate = async () => {
    if (!name.trim()) return;
    setCreating(true);
    try {
      const agent = await api.agents.create({
        name: name.trim(),
        description: description.trim() || undefined,
      });
      router.push(`/dashboard/agents/${agent.id}`);
    } catch {
      setCreating(false);
    }
  };

  return (
    <div className="p-10 space-y-10 relative">
      {/*<div className="flex items-center justify-between">
        <div>
          <div className="flex items-center gap-2 text-muted-foreground mb-1">
            <Link href="/dashboard/agents" className="hover:text-primary transition-colors text-xs">Agents</Link>
            <span className="text-[10px]">/</span>
            <span className="text-foreground font-medium text-xs">New Agent</span>
          </div>
          <h2 className="text-3xl font-bold tracking-tight text-foreground">Deploy New Service Agent</h2>
        </div>
      </div>*/}

      <Link href="/dashboard/agents" className="absolute top-4 right-4 block w-fit">
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
            <h3 className="text-2xl font-bold tracking-tighter">Create Voice Agent</h3>
            <p className="text-muted-foreground mt-1">
              Define the baseline identity for your voice agent.
            </p>
            <HugeiconsIcon
              icon={AudioWave01Icon}
              className="size-52 absolute text-primary/10 -right-8 -bottom-16"
            />
          </div>

          <CardContent className="p-8 pt-4 space-y-8">
            <div className="grid gap-10">
              <div className="space-y-3">
                <label className="block text-xs tracking-wide font-medium uppercase text-muted-foreground">
                  Agent Name
                </label>
                <Input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder="e.g. Premium Support Specialist"
                  className="h-14 text-lg rounded-xl px-6 transition-all"
                  autoFocus
                />
              </div>

              <div className="space-y-3">
                <label className="block text-xs tracking-wide font-medium uppercase text-muted-foreground">
                  Objective (Optional)
                </label>
                <Textarea
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  placeholder="Briefly describe what challenges this agent aims to solve..."
                  className="min-h-[120px] w-full rounded-xl px-6 py-4 transition-all resize-none"
                />
              </div>
            </div>

            <div className="pt-4 space-y-6">
              <Button
                onClick={handleCreate}
                disabled={!name.trim() || creating}
                className="w-full h-14 rounded-xl text-base font-bold shadow-xl shadow-primary/20 gap-3 group"
              >
                {creating ? (
                  <span className="flex items-center gap-2">
                    <Spinner className="size-4" />
                    Initializing ...
                  </span>
                ) : (
                  <>
                    Initialize Voice Agent{" "}
                    <HugeiconsIcon
                      icon={ArrowRight01Icon}
                      className="size-5 group-hover:translate-x-1 transition-transform"
                    />
                  </>
                )}
              </Button>

              <div className="flex items-center gap-3 p-4 rounded-xl bg-muted/50 text-muted-foreground">
                <HugeiconsIcon icon={AiBrowserIcon} className="size-6 shrink-0 text-primary" />
                <p className="text-xs leading-relaxed">
                  Next step: You&apos;ll use the <strong>Agent Builder</strong> to describe the
                  agent&apos;s behavior in natural language.
                </p>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
