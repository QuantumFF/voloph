// React's development build records every component render as a User Timing
// `performance.measure` entry, carrying a DevTools `detail` payload (its
// Performance Tracks). The User Timing buffer holds every entry until it is
// cleared and nothing here reads it, so in the dev app each re-render — the
// analysis-progress ticks, the session-list poll, the mpv playhead — leaks into
// the web process (issue #87: ~1.6 GB/min under a 200 Hz re-render storm).
// Production React emits no measures, so this is installed in dev only.
export function drainPerformanceBuffer(intervalMs = 5_000) {
  const id = setInterval(() => performance.clearMeasures(), intervalMs)
  return () => clearInterval(id)
}
