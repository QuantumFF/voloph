import path from "path"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    // scripts/dev.ts picks a free port so several worktrees can run at once.
    port: Number(process.env.VOLOPH_DEV_PORT ?? 1420),
    strictPort: true,
  },
})
