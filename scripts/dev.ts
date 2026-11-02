#!/usr/bin/env bun
// Launch the app with hot-reload from any checkout: the main repo or a git
// worktree. `bun run app [--isolated] [extra tauri dev args]`
//
// It makes a fresh worktree runnable without manual setup:
//   - finds a desktop display when the shell has none (terminals spawned by
//     editors/agents often lack WAYLAND_DISPLAY / DISPLAY → "Failed to
//     initialize GTK");
//   - installs node_modules (bun install is a no-op when already current);
//   - copies the untracked ffmpeg/ffprobe sidecars from the main checkout, or
//     fetches them when no checkout has them;
//   - picks a free dev-server port, so several worktrees can run at once;
//   - tags the window title with the branch name, to tell the windows apart.
//
// --isolated points the app at <checkout>/.dev-data instead of the real app
// data (~/.local/share/com.quantumff.voloph), for branches that touch the
// schema or whose runs shouldn't mix with real annotations.
import { spawnSync } from "child_process"
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
} from "fs"
import { createServer } from "net"
import path from "path"
import { fileURLToPath } from "url"

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")
const sh = (cmd: string[]) =>
  spawnSync(cmd[0], cmd.slice(1), {
    cwd: root,
    encoding: "utf8",
  }).stdout?.trim() ?? ""
const run = (cmd: string[], env?: Record<string, string>) => {
  const p = spawnSync(cmd[0], cmd.slice(1), {
    cwd: root,
    stdio: "inherit",
    env: { ...process.env, ...env },
  })
  if (p.status !== 0) process.exit(p.status ?? 1)
}

const args = process.argv.slice(2)
const isolated = args.includes("--isolated")
const passthrough = args.filter((a) => a !== "--isolated")

// 1. Display.
const env: Record<string, string> = {}
if (!process.env.WAYLAND_DISPLAY && !process.env.DISPLAY) {
  const runtime =
    process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.()}`
  const wayland = existsSync(runtime)
    ? readdirSync(runtime).find((f) => /^wayland-\d+$/.test(f))
    : undefined
  // X99 and up are headless (Xvfb) displays used for scripted app driving.
  const x11 = existsSync("/tmp/.X11-unix")
    ? readdirSync("/tmp/.X11-unix")
        .map((f) => /^X(\d+)$/.exec(f)?.[1])
        .filter((n): n is string => n !== undefined && Number(n) < 99)
        .sort((a, b) => Number(a) - Number(b))[0]
    : undefined
  if (wayland) env.WAYLAND_DISPLAY = wayland
  else if (x11) env.DISPLAY = `:${x11}`
  else {
    console.error(
      "error: no display found (no WAYLAND_DISPLAY/DISPLAY, no wayland socket, no X server)"
    )
    process.exit(1)
  }
  console.log(
    `[dev] no display in this shell; using ${env.WAYLAND_DISPLAY ?? env.DISPLAY}`
  )
}

// 2. JS dependencies.
run(["bun", "install", "--silent"])

// 3. Sidecars (untracked, so a new worktree has none).
const triple = sh(["rustc", "-vV"]).match(/^host: (.+)$/m)?.[1]
if (!triple) {
  console.error(
    "error: could not determine the Rust host triple (is rustc installed?)"
  )
  process.exit(1)
}
const ext = triple.includes("windows") ? ".exe" : ""
const sidecars = ["ffmpeg", "ffprobe"].map(
  (n) => `src-tauri/binaries/${n}-${triple}${ext}`
)
const missing = sidecars.filter((f) => !existsSync(path.join(root, f)))
const main = sh(["git", "worktree", "list", "--porcelain"]).match(
  /^worktree (.+)$/m
)?.[1]
const inWorktree = main !== undefined && main !== root
if (missing.length) {
  if (inWorktree && missing.every((f) => existsSync(path.join(main, f)))) {
    for (const f of missing) {
      copyFileSync(path.join(main, f), path.join(root, f))
      chmodSync(path.join(root, f), 0o755)
    }
    console.log(`[dev] copied sidecars from ${main}`)
  } else {
    run(["scripts/fetch-sidecars.sh", triple])
  }
}

// 4. Free port, starting at the configured 1420. "localhost" may resolve to
// either family, so the port must be free on both.
const freeOn = (port: number, host: string) =>
  new Promise<boolean>((resolve) => {
    const s = createServer()
      .once("error", (e: NodeJS.ErrnoException) =>
        // No IPv6 on this host: that family can't conflict.
        resolve(e.code === "EADDRNOTAVAIL" || e.code === "EAFNOSUPPORT")
      )
      .once("listening", () => s.close(() => resolve(true)))
      .listen(port, host)
  })
const free = async (port: number) =>
  (await freeOn(port, "127.0.0.1")) && (await freeOn(port, "::1"))
let port = 1420
while (!(await free(port))) port++

// 5. Config override: dev URL on that port, branch in the window title.
const conf = JSON.parse(
  readFileSync(path.join(root, "src-tauri/tauri.conf.json"), "utf8")
)
const branch =
  sh(["git", "branch", "--show-current"]) ||
  sh(["git", "rev-parse", "--short", "HEAD"])
const override = {
  build: { devUrl: `http://localhost:${port}` },
  app: {
    windows: conf.app.windows.map((w: { title: string }) => ({
      ...w,
      title: `${w.title} — ${branch}${isolated ? " (isolated)" : ""}`,
    })),
  },
}

env.VOLOPH_DEV_PORT = String(port)
// Share the main checkout's Rust build dir so a new worktree reuses its
// ~500 compiled dependencies instead of building from scratch; only the app
// crate itself rebuilds when switching worktrees. Cargo releases its build lock
// before the app starts, so concurrent worktree runs only queue on compiling.
if (inWorktree && !process.env.CARGO_TARGET_DIR) {
  env.CARGO_TARGET_DIR = path.join(main, "src-tauri/target")
}
if (isolated) {
  env.XDG_DATA_HOME = path.join(root, ".dev-data")
  mkdirSync(env.XDG_DATA_HOME, { recursive: true })
}
console.log(
  `[dev] ${branch} on port ${port}, data: ${isolated ? env.XDG_DATA_HOME : "real app data"}`
)

run(
  [
    "bun",
    "run",
    "tauri",
    "dev",
    "--config",
    JSON.stringify(override),
    ...passthrough,
  ],
  env
)
