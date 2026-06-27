import path from "path"
import { defineConfig } from "vitest/config"

// Unit tests run in a plain Node environment — the only suite so far covers
// pure transport-decision logic (issue #27), which needs no DOM. Mirrors the
// `@` alias from vite.config.ts so test imports resolve the same way.
export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
})
