import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Only pick up files that explicitly opt in as tests.
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    // Pure logic tests (timeline-reducer, etc.) don't need a DOM.
    environment: "node",
  },
});
