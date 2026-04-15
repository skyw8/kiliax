import React from "react";

export function EmptyState() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <div className="text-xl font-semibold text-zinc-900">Let&apos;s build</div>
        <div className="mt-1 text-sm text-zinc-600">Start typing below to create a session</div>
      </div>
    </div>
  );
}

