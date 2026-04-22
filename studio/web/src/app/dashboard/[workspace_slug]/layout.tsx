"use client";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Robot01Icon,
  CallInternal02Icon,
  DashboardCircleIcon,
  HelpCircleIcon,
  ConnectIcon,
  Settings01Icon,
  VoiceIcon,
} from "@hugeicons/core-free-icons";
import Link from "next/link";
import { useParams, usePathname } from "next/navigation";
import { FerosLogoWordmark } from "@/components/logo";
import { DOCS_URL } from "@/lib/constants";

const navItems = [
  { href: "/dashboard", label: "Home", icon: DashboardCircleIcon },
  { href: "/dashboard/agents", label: "Agents", icon: Robot01Icon },
  { href: "/dashboard/calls", label: "Calls", icon: VoiceIcon },
  {
    href: "/dashboard/phone-numbers",
    label: "Phone Numbers",
    icon: CallInternal02Icon,
  },
  { href: "/dashboard/integrations", label: "Integrations", icon: ConnectIcon },
  { href: "/dashboard/settings", label: "Settings", icon: Settings01Icon },
];

export default function DashboardLayout({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const params = useParams();
  const workspaceSlug = params.workspace_slug as string;

  const getNavHref = (path: string) => {
    return workspaceSlug ? `/dashboard/${workspaceSlug}${path}` : `/dashboard${path}`;
  };

  return (
    <div className="flex min-h-screen bg-background text-foreground">
      {/* Sidebar */}
      <aside className="fixed inset-y-0 left-0 z-50 flex w-60 flex-col bg-sidebar border-r border-sidebar-border">
        {/* Brand */}
        <div className="px-5 pt-5 pb-4">
          <Link href={`/dashboard/${workspaceSlug}`} className="flex items-center gap-2.5">
            <FerosLogoWordmark className="h-7" />
          </Link>
        </div>

        {/* Nav */}
        <nav className="flex-1 px-3 pt-1">
          <div className="space-y-0.5">
            {navItems.map((item) => {
              const href = getNavHref(item.href === "/dashboard" ? "" : item.href.replace("/dashboard", ""));
              const isActive =
                pathname === href ||
                (href !== `/dashboard/${workspaceSlug}` && pathname.startsWith(href));

              return (
                <Link
                  key={href}
                  href={href}
                  className={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition-colors ${
                    isActive
                      ? "bg-sidebar-accent text-sidebar-accent-foreground font-medium"
                      : "text-sidebar-foreground hover:bg-sidebar-muted hover:text-sidebar-accent-foreground"
                  }`}
                >
                  <HugeiconsIcon
                    icon={item.icon}
                    className={`size-4 ${isActive ? "text-primary" : "text-sidebar-muted-foreground"}`}
                  />
                  {item.label}
                </Link>
              );
            })}
          </div>
        </nav>

        {/* Bottom */}
        <div className="p-3 space-y-1">
          <Link
            href={DOCS_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm text-sidebar-foreground hover:bg-sidebar-muted hover:text-sidebar-accent-foreground transition-colors"
          >
            <HugeiconsIcon icon={HelpCircleIcon} className="size-4 text-sidebar-muted-foreground" />
            Docs
          </Link>
        </div>
      </aside>

      {/* Main */}
      <div className="flex-1 ml-60 flex flex-col min-h-screen">
        {/* Content */}
        <main className="flex-1 px-10 py-10 max-w-[1100px] w-full mx-auto">{children}</main>
      </div>
    </div>
  );
}
