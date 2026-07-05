import { StrictMode } from "react"
import { createRoot } from "react-dom/client"

import "./index.css"
import App from "./App.tsx"
import { ThemeProvider } from "@/components/theme-provider.tsx"
import { ExportProvider } from "@/components/use-export.ts"
import { ExternalLinkGuard } from "./components/external-link-guard.tsx"
import { DebugPanel } from "./components/debug-panel.tsx"

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <ExternalLinkGuard />
      {import.meta.env.DEV ? <DebugPanel /> : null}
      <ExportProvider>
        <main data-ui-scroll-container><App /></main>
      </ExportProvider>
    </ThemeProvider>
  </StrictMode>
)
