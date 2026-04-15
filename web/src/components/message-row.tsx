import React from "react";
import { Copy, GitFork, MoreHorizontal, Pencil, RefreshCcw } from "lucide-react";

import {
  copyToClipboard,
  fmtDurationCompact,
  fmtTokenUsage,
  hasMermaidFence,
  parseMessageId,
} from "../lib/app-utils";
import type { Message, ToolCall } from "../lib/types";
import { CodeBlock } from "./code-block";
import { Markdown, type MermaidErrorInfo } from "./markdown";

function renderToolCalls(
  toolCalls: ToolCall[] | undefined,
  toolDurationsMs: Record<string, number>,
) {
  if (!toolCalls?.length) return null;
  return (
    <div className="mt-2 space-y-1">
      {toolCalls.map((c) => (
        <details
          key={c.id}
          className="relative rounded-md border border-zinc-200 bg-white px-3 py-2"
        >
          <button
            type="button"
            className="absolute right-1 top-1 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
            aria-label="Copy tool call"
            title="Copy tool call"
            onClick={() => copyToClipboard(c.arguments)}
          >
            <Copy className="h-3.5 w-3.5" />
          </button>
          <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-700">
            tool_call: <span className="font-mono">{c.name}</span>
            {toolDurationsMs[c.id] != null ? (
              <span className="ml-2 text-zinc-500">
                ({fmtDurationCompact(toolDurationsMs[c.id]!)})
              </span>
            ) : null}
          </summary>
          <CodeBlock className="mt-2" code={c.arguments} lang="json" copyButton={false} />
        </details>
      ))}
    </div>
  );
}

export function MessageRow({
  msg,
  toolDurationsMs,
  thinkingDurationsMs,
  assistantDurationsMs,
  onMermaidError,
  onFork,
  onEditUser,
  onRegenerateAssistant,
  historyMutable,
}: {
  msg: Message;
  toolDurationsMs: Record<string, number>;
  thinkingDurationsMs: Record<string, number>;
  assistantDurationsMs: Record<string, number>;
  onMermaidError?: (info: MermaidErrorInfo) => void;
  onFork?: (assistantMessageId: string) => void;
  onEditUser?: (userMessageId: string, content: string) => void;
  onRegenerateAssistant?: (assistantMessageId: string) => void;
  historyMutable?: boolean;
}) {
  if (msg.role === "user") {
    const wide = hasMermaidFence(msg.content);
    const bubbleWidth = wide ? "w-full max-w-[92%]" : "max-w-[92%] sm:max-w-[78%]";
    const canEdit = Boolean(historyMutable && onEditUser && parseMessageId(msg.id));
    return (
      <div className="group flex justify-end">
        <div className={`${bubbleWidth} relative whitespace-pre-wrap break-words rounded-2xl bg-zinc-900 px-4 py-2 text-sm text-zinc-50`}>
          <div className="absolute right-full top-2 flex items-center gap-1 pr-2 invisible opacity-0 transition-opacity group-hover:visible group-hover:opacity-100">
            <button
              type="button"
              className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
              aria-label="Copy message"
              title="Copy message"
              onClick={() => copyToClipboard(msg.content)}
            >
              <Copy className="h-4 w-4" />
            </button>
            <button
              type="button"
              disabled={!canEdit}
              className={[
                "rounded-md p-1 text-zinc-500",
                canEdit ? "hover:bg-zinc-100" : "cursor-not-allowed opacity-40",
              ].join(" ")}
              aria-label="Edit message"
              title="Edit message"
              onClick={() => onEditUser?.(msg.id, msg.content)}
            >
              <Pencil className="h-4 w-4" />
            </button>
          </div>

          {msg.content}
        </div>
      </div>
    );
  }

  if (msg.role === "assistant") {
    const wide = hasMermaidFence(msg.content);
    const bubbleWidth = wide ? "w-full max-w-[92%]" : "max-w-[92%] sm:max-w-[78%]";
    const usageText = fmtTokenUsage(msg.usage);
    const canRegenerate = Boolean(historyMutable && onRegenerateAssistant);
    return (
      <div className="flex justify-start">
        <div className={`${bubbleWidth} rounded-2xl bg-zinc-50 px-4 py-2 text-sm text-zinc-900`}>
          {msg.content ? (
            <Markdown text={msg.content} messageId={msg.id} onMermaidError={onMermaidError} />
          ) : (
            <div className="text-zinc-500">…</div>
          )}
          {msg.reasoning_content ? (
            <details className="relative mt-2 rounded-md border border-zinc-200 bg-zinc-50 px-3 py-2">
              <button
                type="button"
                className="absolute right-1 top-1 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                aria-label="Copy thinking"
                title="Copy thinking"
                onClick={() => copyToClipboard(msg.reasoning_content ?? "")}
              >
                <Copy className="h-3.5 w-3.5" />
              </button>
              <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-600">
                thinking
                {thinkingDurationsMs[msg.id] != null ? (
                  <span className="ml-2 text-zinc-500">
                    ({fmtDurationCompact(thinkingDurationsMs[msg.id]!)})
                  </span>
                ) : null}
              </summary>
              <div className="mt-2 whitespace-pre-wrap text-xs italic text-zinc-700">
                {msg.reasoning_content}
              </div>
            </details>
          ) : null}
          {renderToolCalls(msg.tool_calls ?? [], toolDurationsMs)}
          {usageText ? (
            <div className="mt-2 truncate text-xs text-zinc-500" title={usageText}>
              {usageText}
            </div>
          ) : null}

          <div className="mt-2 border-t border-zinc-200 pt-1">
            <div className="flex items-center gap-1">
              <button
                type="button"
                className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                aria-label="Copy message"
                title="Copy message"
                onClick={() => copyToClipboard(msg.content ?? "")}
              >
                <Copy className="h-4 w-4" />
              </button>
              <button
                type="button"
                disabled={!canRegenerate}
                className={[
                  "rounded-md p-1 text-zinc-500",
                  canRegenerate ? "hover:bg-zinc-100" : "cursor-not-allowed opacity-40",
                ].join(" ")}
                aria-label="Regenerate"
                title="Regenerate"
                onClick={() => onRegenerateAssistant?.(msg.id)}
              >
                <RefreshCcw className="h-4 w-4" />
              </button>
              <button
                type="button"
                disabled={!onFork}
                className={[
                  "rounded-md p-1 text-zinc-500",
                  onFork ? "hover:bg-zinc-100" : "cursor-not-allowed opacity-40",
                ].join(" ")}
                aria-label="Fork session"
                title="Fork session from here"
                onClick={() => onFork?.(msg.id)}
              >
                <GitFork className="h-4 w-4" />
              </button>
              <button
                type="button"
                className="rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
                aria-label="Menu"
                title="Menu"
              >
                <MoreHorizontal className="h-4 w-4" />
              </button>
              {assistantDurationsMs[msg.id] != null ? (
                <span className="ml-auto text-xs text-zinc-500">
                  {fmtDurationCompact(assistantDurationsMs[msg.id]!)}
                </span>
              ) : null}
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex justify-start">
      <details className="relative w-full max-w-[92%] rounded-2xl border border-zinc-200 bg-zinc-50 px-4 py-2 sm:max-w-[78%]">
        <button
          type="button"
          className="absolute right-2 top-2 rounded-md p-1 text-zinc-500 hover:bg-zinc-100"
          aria-label="Copy tool result"
          title="Copy tool result"
          onClick={() => copyToClipboard(msg.content ?? "")}
        >
          <Copy className="h-3.5 w-3.5" />
        </button>
        <summary className="cursor-pointer select-none pr-10 text-xs text-zinc-700">
          tool_result: <span className="font-mono">{msg.tool_call_id}</span>
          {toolDurationsMs[msg.tool_call_id] != null ? (
            <span className="ml-2 text-zinc-500">
              ({fmtDurationCompact(toolDurationsMs[msg.tool_call_id]!)})
            </span>
          ) : null}
        </summary>
        <CodeBlock className="mt-2" code={msg.content} lang="json" copyButton={false} />
      </details>
    </div>
  );
}

