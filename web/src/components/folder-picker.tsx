import React, { useEffect, useRef, useState } from "react";
import { ArrowLeft, ChevronRight, RefreshCcw } from "lucide-react";

import { api, ApiError } from "../lib/api";
import type { FsEntry } from "../lib/types";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";

export function FolderPicker({
  path,
  onPathChange,
}: {
  path: string;
  onPathChange: (next: string) => void;
}) {
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [parent, setParent] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadSeq, setReloadSeq] = useState(0);
  const skipNextFetchPathRef = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const handle = window.setTimeout(() => {
      if (cancelled) return;
      async function load() {
        if (skipNextFetchPathRef.current === path) {
          skipNextFetchPathRef.current = null;
          return;
        }

        setLoading(true);
        setError(null);
        try {
          const res = await api.fsList(path.trim() ? path.trim() : undefined);
          if (cancelled) return;
          setEntries(res.entries ?? []);
          setParent(res.parent ?? null);
          setLoading(false);
          if (res.path && res.path !== path) {
            skipNextFetchPathRef.current = res.path;
            onPathChange(res.path);
          }
        } catch (err) {
          if (cancelled) return;
          setLoading(false);
          const msg =
            err instanceof ApiError
              ? err.message
              : err instanceof Error
                ? err.message
                : "Failed to list folders";
          setError(msg);
        }
      }
      void load();
    }, 150);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [path, reloadSeq, onPathChange]);

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 rounded-md border border-zinc-200 bg-white px-2 py-1">
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          aria-label="Up"
          title="Up"
          disabled={!parent || loading}
          onClick={() => {
            if (!parent) return;
            onPathChange(parent);
          }}
        >
          <ArrowLeft className="h-4 w-4 text-zinc-600" />
        </Button>
        <Input
          value={path}
          onChange={(e) => onPathChange(e.target.value)}
          placeholder="/path/to/folder"
          aria-label="Path"
          title={path}
          className="h-8 min-w-0 flex-1 px-2 py-1 font-mono text-xs"
        />
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          aria-label="Refresh"
          title="Refresh"
          disabled={loading}
          onClick={() => setReloadSeq((v) => v + 1)}
        >
          <RefreshCcw className="h-4 w-4 text-zinc-600" />
        </Button>
      </div>

      {error ? (
        <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
          {error}
        </div>
      ) : null}

      <div className="h-[min(320px,50vh)] overflow-auto rounded-md border border-zinc-200 bg-white">
        {loading ? (
          <div className="flex h-full items-center justify-center px-3 text-center text-xs text-zinc-500">
            Loading…
          </div>
        ) : entries.length ? (
          <div className="divide-y divide-zinc-200">
            {entries.map((e) => (
              <button
                key={e.path}
                type="button"
                className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-zinc-50"
                onClick={() => onPathChange(e.path)}
              >
                <ChevronRight className="h-4 w-4 shrink-0 text-zinc-500" />
                <div className="min-w-0 flex-1 truncate font-mono text-xs text-zinc-800">
                  {e.name}
                </div>
              </button>
            ))}
          </div>
        ) : (
          <div className="flex h-full items-center justify-center px-3 text-center text-xs text-zinc-500">
            No folders
          </div>
        )}
      </div>
    </div>
  );
}

export function FolderPickerDialog({
  open,
  onOpenChange,
  title,
  description,
  path,
  onPathChange,
  confirmLabel,
  confirmDisabled,
  confirmPending,
  confirmPendingLabel,
  onConfirm,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  path: string;
  onPathChange: (next: string) => void;
  confirmLabel: string;
  confirmDisabled?: boolean;
  confirmPending?: boolean;
  confirmPendingLabel?: string;
  onConfirm: () => void | Promise<void>;
  children?: React.ReactNode;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        <div className="space-y-3">
          {children}
          <FolderPicker path={path} onPathChange={onPathChange} />
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={onConfirm} disabled={confirmDisabled}>
              {confirmPending ? (confirmPendingLabel ?? confirmLabel) : confirmLabel}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

