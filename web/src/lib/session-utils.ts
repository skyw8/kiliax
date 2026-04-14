import type { SessionSummary } from "./types";

export function statusBadge(summary: SessionSummary): {
  label: string;
  variant: "idle" | "step" | "done" | "error";
} {
  const runState = summary.status.run_state;
  if (runState === "running" || runState === "tooling") {
    const step = summary.status.step > 0 ? summary.status.step : 1;
    return { label: `step ${step}`, variant: "step" };
  }
  if (summary.last_outcome === "error") return { label: "error", variant: "error" };
  if (summary.last_outcome === "done") return { label: "done", variant: "done" };
  return { label: "idle", variant: "idle" };
}

export function sortSessions(items: SessionSummary[], pinnedIds: string[]): SessionSummary[] {
  const pinnedRank = new Map(pinnedIds.map((id, idx) => [id, idx]));
  return [...items].sort((a, b) => {
    const aPinned = pinnedRank.has(a.id);
    const bPinned = pinnedRank.has(b.id);
    if (aPinned !== bPinned) return aPinned ? -1 : 1;
    if (aPinned && bPinned) {
      return (pinnedRank.get(a.id) ?? 0) - (pinnedRank.get(b.id) ?? 0);
    }

    const aRunning =
      a.status.run_state === "running" || a.status.run_state === "tooling";
    const bRunning =
      b.status.run_state === "running" || b.status.run_state === "tooling";
    if (aRunning !== bRunning) return aRunning ? -1 : 1;
    return b.updated_at.localeCompare(a.updated_at);
  });
}

