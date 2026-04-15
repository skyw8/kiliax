import { AlertTriangle, CheckCircle2, X } from "lucide-react";

import { stringifyUnknown } from "../lib/app-utils";
import { Alert } from "./ui/alert";

export type AlertItem = {
  id: string;
  title: string;
  subtitle?: string;
  message: string;
  traceId?: string;
  details?: unknown;
  autoCloseMs?: number;
  level?: "success" | "error";
};

export function AlertStack({
  items,
  onClose,
}: {
  items: AlertItem[];
  onClose: (id: string) => void;
}) {
  if (!items.length) return null;

  return (
    <div className="fixed bottom-6 right-6 z-[1000] flex w-[min(560px,calc(100vw-24px))] flex-col gap-3">
      {items.map((a) => (
        <Alert
          key={a.id}
          variant={a.level === "success" ? "success" : "destructive"}
          className="shadow-lg"
        >
          <div className="flex items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-2">
              {a.level === "success" ? (
                <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-emerald-600" />
              ) : (
                <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-red-600" />
              )}
              <div className="min-w-0">
                <div className="truncate text-sm font-semibold text-zinc-900">
                  {a.title}
                </div>
                {a.subtitle ? (
                  <div className="mt-0.5 truncate text-xs text-zinc-600">
                    {a.subtitle}
                  </div>
                ) : null}
              </div>
            </div>
            <button
              type="button"
              className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
              aria-label="Close"
              onClick={() => onClose(a.id)}
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          <pre className="mt-3 max-h-[32vh] overflow-auto whitespace-pre-wrap rounded-md bg-zinc-50 p-3 text-xs text-zinc-800">
            {a.message}
          </pre>

          {a.traceId ? (
            <div className="mt-2 text-xs text-zinc-600">
              trace_id: <span className="font-mono">{a.traceId}</span>
            </div>
          ) : null}

          {a.details != null ? (
            <details className="mt-2 rounded-md border border-zinc-200 bg-white px-3 py-2">
              <summary className="cursor-pointer select-none text-xs text-zinc-700">
                details
              </summary>
              <pre className="mt-2 max-h-[32vh] overflow-auto rounded bg-zinc-50 p-2 text-xs text-zinc-800">
                {stringifyUnknown(a.details)}
              </pre>
            </details>
          ) : null}
        </Alert>
      ))}
    </div>
  );
}
