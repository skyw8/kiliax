import React from "react";
import { MoreHorizontal, Pin } from "lucide-react";

import { modelLabel } from "../lib/app-utils";
import { statusBadge } from "../lib/session-utils";
import type { SessionSummary } from "../lib/types";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";

export function SessionItemRow({
  summary,
  active,
  pinned,
  onSelect,
  onOpenMenu,
}: {
  summary: SessionSummary;
  active: boolean;
  pinned: boolean;
  onSelect: () => void;
  onOpenMenu: (sessionId: string, anchor: DOMRect) => void;
}) {
  const badge = statusBadge(summary);
  return (
    <div
      className={[
        "group flex items-start gap-1 rounded-md px-2 py-2",
        active ? "bg-white shadow-sm" : "hover:bg-white/70",
      ].join(" ")}
    >
      <button onClick={onSelect} className="min-w-0 flex-1 text-left">
        <div className="flex items-center justify-between gap-2">
          <div className="min-w-0 flex items-center gap-1 text-sm text-zinc-900">
            {pinned ? <Pin className="h-3.5 w-3.5 shrink-0 text-violet-600" /> : null}
            <div className="truncate">{summary.title || summary.id}</div>
          </div>
          <Badge variant={badge.variant}>{badge.label}</Badge>
        </div>
        <div className="mt-1 truncate text-xs text-zinc-500">
          {modelLabel(summary.settings.model_id)}
        </div>
      </button>

      <Button
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        aria-label="Session actions"
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          const rect = (e.currentTarget as HTMLButtonElement).getBoundingClientRect();
          onOpenMenu(summary.id, rect);
        }}
      >
        <MoreHorizontal className="h-4 w-4 text-zinc-500" />
      </Button>
    </div>
  );
}

