import React from "react";

import { cn } from "../../lib/utils";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "./sheet";

export function ActionSheet({
  open,
  onOpenChange,
  title,
  description,
  children,
  className,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title?: string;
  description?: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="bottom"
        showClose={false}
        className={cn("bg-white pb-[env(safe-area-inset-bottom)]", className)}
      >
        <div className="px-4 pb-4 pt-3">
          <div className="mx-auto mb-3 h-1.5 w-12 rounded-full bg-zinc-200" aria-hidden="true" />
          {title ? (
            <SheetTitle className="text-sm">{title}</SheetTitle>
          ) : null}
          {description ? (
            <SheetDescription className="mt-1 text-sm">{description}</SheetDescription>
          ) : null}
          <div className={cn("mt-3", title || description ? "" : "mt-0")}>
            {children}
          </div>
        </div>
      </SheetContent>
    </Sheet>
  );
}
