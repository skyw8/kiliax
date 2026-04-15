import React from "react";

import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";

export function DeleteSessionDialog(props: {
  open: boolean;
  onClose: () => void;
  description: string;
  showTmpWorkspaceWarning: boolean;
  onDelete: () => Promise<void>;
}) {
  const { open, onClose, description, showTmpWorkspaceWarning, onDelete } = props;

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete session?</DialogTitle>
          <DialogDescription className="truncate">{description}</DialogDescription>
        </DialogHeader>
        {showTmpWorkspaceWarning ? (
          <div className="mt-3 text-sm text-zinc-600">
            This also deletes the temporary workspace directory on disk.
          </div>
        ) : null}
        <div className="mt-3 flex justify-end gap-2">
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button
            className="bg-red-600 text-zinc-50 hover:bg-red-500"
            onClick={async () => {
              onClose();
              await onDelete();
            }}
          >
            Delete
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

