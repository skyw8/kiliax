import React from "react";
import { Flag, GitFork, Info, Pin, Trash2, XCircle } from "lucide-react";

import { ActionSheet } from "./ui/action-sheet";
import { Button } from "./ui/button";

export function SessionActionSheet(props: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  description: string;
  pinLabel: string;
  onFork: () => void;
  onSetGoal: () => void;
  onGoalInfo: () => void;
  onClearGoal: () => void;
  onTogglePinned: () => void;
  onDelete: () => void;
}) {
  const { open, onOpenChange, description, pinLabel, onFork, onSetGoal, onGoalInfo, onClearGoal, onTogglePinned, onDelete } = props;

  return (
    <ActionSheet
      open={open}
      onOpenChange={onOpenChange}
      title="Session actions"
      description={description}
    >
      <div className="space-y-2">
        <button
          type="button"
          className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
          onClick={onFork}
        >
          <GitFork className="h-5 w-5 text-violet-600" />
          Fork
        </button>
        <button
          type="button"
          className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
          onClick={onSetGoal}
        >
          <Flag className="h-5 w-5 text-emerald-600" />
          Set Goal
        </button>
        <button
          type="button"
          className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
          onClick={onGoalInfo}
        >
          <Info className="h-5 w-5 text-blue-600" />
          Goal Info
        </button>
        <button
          type="button"
          className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
          onClick={onClearGoal}
        >
          <XCircle className="h-5 w-5 text-zinc-500" />
          Clear Goal
        </button>
        <button
          type="button"
          className="flex w-full items-center gap-3 rounded-md border border-zinc-200 bg-white px-3 py-3 text-left text-base text-zinc-900 active:opacity-80"
          onClick={onTogglePinned}
        >
          <Pin className="h-5 w-5 text-violet-600" />
          {pinLabel}
        </button>
        <button
          type="button"
          className="flex w-full items-center gap-3 rounded-md border border-rose-200 bg-white px-3 py-3 text-left text-base text-rose-700 active:opacity-80"
          onClick={onDelete}
        >
          <Trash2 className="h-5 w-5" />
          Delete
        </button>
        <Button variant="outline" className="w-full" onClick={() => onOpenChange(false)}>
          Cancel
        </Button>
      </div>
    </ActionSheet>
  );
}

export function SessionContextMenu(props: {
  open: boolean;
  menuRef: React.RefObject<HTMLDivElement>;
  x: number;
  y: number;
  pinLabel: string;
  onFork: () => void;
  onSetGoal: () => void;
  onGoalInfo: () => void;
  onClearGoal: () => void;
  onTogglePinned: () => void;
  onDelete: () => void;
}) {
  const { open, menuRef, x, y, pinLabel, onFork, onSetGoal, onGoalInfo, onClearGoal, onTogglePinned, onDelete } = props;

  if (!open) return null;

  return (
    <div
      ref={menuRef}
      style={{ left: x, top: y }}
      className="fixed z-50 mt-1 w-44 -translate-x-full rounded-md border border-zinc-200 bg-white p-1 shadow-lg"
    >
      <button
        className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
        onClick={onFork}
      >
        <GitFork className="h-4 w-4 text-violet-600" />
        Fork
      </button>
      <button
        className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
        onClick={onSetGoal}
      >
        <Flag className="h-4 w-4 text-emerald-600" />
        Set Goal
      </button>
      <button
        className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
        onClick={onGoalInfo}
      >
        <Info className="h-4 w-4 text-blue-600" />
        Goal Info
      </button>
      <button
        className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
        onClick={onClearGoal}
      >
        <XCircle className="h-4 w-4 text-zinc-500" />
        Clear Goal
      </button>
      <button
        className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-zinc-800 hover:bg-zinc-100"
        onClick={onTogglePinned}
      >
        <Pin className="h-4 w-4 text-violet-600" />
        {pinLabel}
      </button>
      <button
        className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm text-red-600 hover:bg-red-50"
        onClick={onDelete}
      >
        <Trash2 className="h-4 w-4" />
        Delete
      </button>
    </div>
  );
}
