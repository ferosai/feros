"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
export default function DashboardRootRedirect() {
  const router = useRouter();
  useEffect(() => {
    // Fallback for OSS: always redirect to the default workspace slug.
    router.replace("/dashboard/default");
  }, [
    router,
  ]);

  return (
    <div className="flex h-screen w-screen items-center justify-center bg-background">
      <div className="size-6 animate-spin rounded-full border-2 border-primary border-t-transparent" />
    </div>
  );
}
