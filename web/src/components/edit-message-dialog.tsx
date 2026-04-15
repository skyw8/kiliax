import React from "react";

import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";
import { Textarea } from "./ui/textarea";

export function EditMessageDialog(props: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  draft: string;
  onDraftChange: (draft: string) => void;
  saving: boolean;
  canSave: boolean;
  onCancel: () => void;
  onSave: () => void;
}) {
  const { open, onOpenChange, draft, onDraftChange, saving, canSave, onCancel, onSave } = props;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Edit message</DialogTitle>
          <DialogDescription>Edits history and regenerates from here.</DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <Textarea
            value={draft}
            onChange={(e) => onDraftChange(e.target.value)}
            className="min-h-[140px] resize-none"
            placeholder="Update the user message…"
          />
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={onCancel} disabled={saving}>
              Cancel
            </Button>
            <Button onClick={onSave} disabled={!canSave}>
              {saving ? "Saving…" : "Save & Send"}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

