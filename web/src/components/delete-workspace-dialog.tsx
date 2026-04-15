import React from "react";

import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";

export function DeleteWorkspaceDialog(props: {
  open: boolean;
  onClose: () => void;
  workspaceRoot: string;
  isTmpWorkspaceRoot: boolean;
  onDelete: () => Promise<void>;
}) {
  const { open, onClose, workspaceRoot, isTmpWorkspaceRoot, onDelete } = props;

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete workspace?</DialogTitle>
          <DialogDescription className="truncate">{workspaceRoot}</DialogDescription>
        </DialogHeader>
        <div className="mt-3 text-sm text-zinc-600">
          {isTmpWorkspaceRoot ? (
            <>This deletes all sessions and removes the temporary workspace directory on disk.</>
          ) : (
            <>This deletes all sessions under this workspace (directory is not removed).</>
          )}
        </div>
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

