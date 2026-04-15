import React, { useState } from "react";

import type { Capabilities, Session } from "../lib/types";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";

export function McpDialog(props: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  session: Session | null;
  capabilities: Capabilities | null;
  patchSession: (patch: any) => Promise<boolean>;
  saveSelectedSessionDefaults: (
    req: { model: boolean; agent?: boolean; mcp: boolean; skills?: boolean },
    message: string,
  ) => Promise<boolean>;
}) {
  const { open, onOpenChange, session, capabilities, patchSession, saveSelectedSessionDefaults } = props;

  const [saving, setSaving] = useState(false);
  const [defaultsSaving, setDefaultsSaving] = useState(false);

  const servers = session?.mcp_status ?? capabilities?.mcp_servers ?? [];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>MCP</DialogTitle>
          <DialogDescription>Current session MCP servers</DialogDescription>
        </DialogHeader>
        <div className="space-y-2">
          {servers.map((s) => (
            <label
              key={s.id}
              className="flex items-center justify-between rounded-md border border-zinc-200 bg-white px-3 py-2"
            >
              <div className="min-w-0">
                <div className="truncate text-sm">{s.id}</div>
                <div className="mt-0.5 truncate text-xs text-zinc-600">
                  {s.state?.toString() ?? "unknown"}
                  {s.last_error ? ` · ${s.last_error}` : ""}
                </div>
              </div>
              <input
                type="checkbox"
                checked={s.enable}
                disabled={saving || !session}
                onChange={async (e) => {
                  if (!session) return;
                  const next = e.target.checked;
                  setSaving(true);
                  try {
                    await patchSession({
                      mcp: { servers: [{ id: s.id, enable: next }] },
                    });
                  } finally {
                    setSaving(false);
                  }
                }}
              />
            </label>
          ))}

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
                    { model: false, mcp: true },
                    "Saved current session MCP enablement as the default.",
                  );
                } finally {
                  setDefaultsSaving(false);
                }
              }}
            >
              Save MCP defaults
            </Button>
          </div>

          {!servers.length ? (
            <div className="text-center text-sm text-zinc-500">No MCP servers</div>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}

