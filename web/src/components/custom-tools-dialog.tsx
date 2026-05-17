import React, { useEffect, useRef, useState } from "react";

import { api } from "../lib/api";
import type {
  CustomToolLoadError,
  CustomToolSummary,
  Session,
  SessionSummary,
} from "../lib/types";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";

function readCustomToolsSettings(session: Session | null, summary: SessionSummary | null): {
  defaultEnable: boolean;
  overrides: Record<string, boolean>;
} {
  const settings = session?.settings.custom_tools ?? summary?.settings.custom_tools;
  const defaultEnable = settings?.default_enable ?? true;
  const overrides: Record<string, boolean> = {};
  for (const o of settings?.overrides ?? []) {
    if (!o?.id) continue;
    overrides[o.id] = Boolean(o.enable);
  }
  return { defaultEnable, overrides };
}

export function CustomToolsDialog(props: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  selectedSessionId: string | null;
  session: Session | null;
  sessionSummary: SessionSummary | null;
  patchSession: (patch: any) => Promise<boolean>;
  saveSelectedSessionDefaults: (
    req: { model: boolean; agent?: boolean; mcp: boolean; skills?: boolean; custom_tools?: boolean },
    message: string,
  ) => Promise<boolean>;
  onApiError: (err: unknown) => void;
}) {
  const {
    open,
    onOpenChange,
    selectedSessionId,
    session,
    sessionSummary,
    patchSession,
    saveSelectedSessionDefaults,
    onApiError,
  } = props;

  const onApiErrorRef = useRef(onApiError);
  onApiErrorRef.current = onApiError;

  const sessionRef = useRef(session);
  sessionRef.current = session;

  const sessionSummaryRef = useRef(sessionSummary);
  sessionSummaryRef.current = sessionSummary;

  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [tools, setTools] = useState<CustomToolSummary[]>([]);
  const [loadErrors, setLoadErrors] = useState<CustomToolLoadError[]>([]);
  const [defaultEnable, setDefaultEnable] = useState(true);
  const [overrides, setOverrides] = useState<Record<string, boolean>>({});
  const [saving, setSaving] = useState(false);
  const [defaultsSaving, setDefaultsSaving] = useState(false);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);

    (async () => {
      try {
        const res = selectedSessionId
          ? await api.listCustomTools(selectedSessionId)
          : await api.listGlobalCustomTools();
        if (cancelled) return;
        setTools(res.items ?? []);
        setLoadErrors(res.errors ?? []);
        const next = readCustomToolsSettings(sessionRef.current, sessionSummaryRef.current);
        setDefaultEnable(next.defaultEnable);
        setOverrides(next.overrides);
      } catch (err) {
        onApiErrorRef.current(err);
      } finally {
        if (!cancelled) {
          setLoaded(true);
          setLoading(false);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [open, selectedSessionId]);

  const showInitialLoading = loading && !loaded;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Custom Tools</DialogTitle>
          <DialogDescription>
            {session ? "Current session custom tools" : "Select a session to edit custom tools"}
          </DialogDescription>
        </DialogHeader>

        {showInitialLoading ? (
          <div className="py-10 text-center text-sm text-zinc-500">Loading...</div>
        ) : (
          <div className="space-y-2">
            {loadErrors.length ? (
              <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
                <div className="font-medium">Some custom tools failed to load</div>
                <div className="mt-1 max-h-[180px] space-y-2 overflow-auto pr-1">
                  {loadErrors.slice(0, 6).map((e) => (
                    <div
                      key={`${e.id}:${e.path}`}
                      className="rounded-md border border-amber-200 bg-white/70 px-2 py-1"
                    >
                      <div className="font-mono text-xs text-amber-900">{e.id}</div>
                      <div className="mt-0.5 break-all font-mono text-[11px] text-amber-700">
                        {e.path}
                      </div>
                      <div className="mt-0.5 whitespace-pre-wrap break-words text-[11px] text-amber-900">
                        {e.error}
                      </div>
                    </div>
                  ))}
                  {loadErrors.length > 6 ? (
                    <div className="text-amber-700">+{loadErrors.length - 6} more...</div>
                  ) : null}
                </div>
              </div>
            ) : null}

            <label className="flex items-center justify-between rounded-md border border-zinc-200 bg-white px-3 py-2">
              <div className="text-sm text-zinc-900">Enable by default</div>
              <input
                type="checkbox"
                checked={defaultEnable}
                disabled={saving || !session}
                onChange={async (e) => {
                  const next = e.target.checked;
                  const prev = defaultEnable;
                  setDefaultEnable(next);
                  setSaving(true);
                  try {
                    const ok = await patchSession({ custom_tools: { default_enable: next } });
                    if (!ok) setDefaultEnable(prev);
                  } finally {
                    setSaving(false);
                  }
                }}
              />
            </label>

            <div className="max-h-[360px] overflow-auto rounded-md border border-zinc-200">
              {tools.length ? (
                <div className="divide-y divide-zinc-200">
                  {tools.map((tool) => {
                    const enabled = overrides[tool.id] ?? defaultEnable;
                    return (
                      <label
                        key={tool.id}
                        className="flex items-center justify-between gap-3 bg-white px-3 py-2"
                      >
                        <div className="min-w-0">
                          <div className="truncate text-sm font-medium text-zinc-900">
                            {tool.name}
                          </div>
                          <div className="mt-0.5 truncate text-xs text-zinc-600">
                            <span className="font-mono">{tool.id}</span>
                            {tool.description ? ` · ${tool.description}` : ""}
                          </div>
                        </div>
                        <input
                          type="checkbox"
                          checked={enabled}
                          disabled={saving || !session}
                          onChange={async (e) => {
                            if (!session) return;
                            const next = e.target.checked;
                            const prev = overrides[tool.id];
                            setOverrides((o) => ({ ...o, [tool.id]: next }));
                            setSaving(true);
                            try {
                              const ok = await patchSession({
                                custom_tools: { overrides: [{ id: tool.id, enable: next }] },
                              });
                              if (!ok) {
                                setOverrides((o) => {
                                  const copy = { ...o };
                                  if (prev === undefined) delete copy[tool.id];
                                  else copy[tool.id] = prev;
                                  return copy;
                                });
                              }
                            } finally {
                              setSaving(false);
                            }
                          }}
                        />
                      </label>
                    );
                  })}
                </div>
              ) : (
                <div className="bg-white px-3 py-6 text-center text-sm text-zinc-500">
                  No custom tools
                </div>
              )}
            </div>

            <div className="flex justify-end">
              <Button
                variant="outline"
                size="sm"
                disabled={!session || defaultsSaving}
                onClick={async () => {
                  if (!session) return;
                  setDefaultsSaving(true);
                  try {
                    await saveSelectedSessionDefaults(
                      { model: false, mcp: false, custom_tools: true },
                      "Saved current session custom tools as the default.",
                    );
                  } finally {
                    setDefaultsSaving(false);
                  }
                }}
              >
                Save custom tool defaults
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
