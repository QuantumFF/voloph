import path from "path"
import { defineConfig } from "vitest/config"

// Unit tests default to a plain Node environment — most suites cover pure
// transport-decision logic (issue #27), which needs no DOM. Hook-level suites
// (e.g. the playback-orchestration race tests) opt into jsdom per file via the
// `@vitest-environment jsdom` pragma. Mirrors the `@` alias from vite.config.ts
// so test imports resolve the same way.
export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
})
