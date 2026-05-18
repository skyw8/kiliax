import React from "react";

export function EmptyState() {
  return (
    <div className="grid min-h-full w-full place-items-center px-4">
      <div className="text-center">
        <div className="text-xl font-semibold text-zinc-900">Let&apos;s cook</div>
        <div className="mt-1 text-sm text-zinc-600">Start typing below to create a session</div>
      </div>
    </div>
  );
}
