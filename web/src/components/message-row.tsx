import React from "react";
import {
  ChevronDown,
  ChevronUp,
  Copy,
  FileText,
  GitFork,
  Image as ImageIcon,
  MoreHorizontal,
  Pencil,
  RefreshCcw,
} from "lucide-react";

import {
  copyToClipboard,
  fmtDurationCompact,
  fmtTokenUsage,
  hasMermaidFence,
  parseMessageId,
} from "../lib/app-utils";
import type { Message, MessageAttachment, ToolCall } from "../lib/types";
import { CodeBlock } from "./code-block";
import { Markdown, type MermaidErrorInfo } from "./markdown";

const USER_COLLAPSE_CHAR_LIMIT = 700;
const USER_COLLAPSE_LINE_LIMIT = 10;

function shouldCollapseUserMessage(content: string): boolean {
  if (content.length > USER_COLLAPSE_CHAR_LIMIT) return true;
  return content.split(/\r\n|\r|\n/).length > USER_COLLAPSE_LINE_LIMIT;
}

function renderUserAttachments(
  attachments: MessageAttachment[] | undefined,
  dark: boolean,
  hasContent: boolean,
) {
  if (!attachments?.length) return null;
  return (
    <div className={`${hasContent ? "mt-2" : ""} flex flex-wrap gap-1.5`}>
      {attachments.map((a, idx) => {
        const isPdf = a.media_type === "application/pdf";
        const Icon = isPdf ? FileText : ImageIcon;
        return (
          <div
            key={`${a.filename}-${idx}`}
            className={[
              "flex max-w-full items-center gap-1.5 rounded-full border px-2 py-1 text-xs",
              dark
                ? "border-zinc-700 bg-zinc-800 text-zinc-100"
                : "border-zinc-300 bg-zinc-200 text-zinc-800",
            ].join(" ")}
            title={a.filename}
          >
            <Icon className={["h-3.5 w-3.5 shrink-0", dark ? "text-zinc-300" : "text-zinc-600"].join(" ")} />
            <span className="max-w-[220px] truncate">{a.filename}</span>
          </div>
        );
      })}
    </div>
  );
}

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
  const [userMessageExpanded, setUserMessageExpanded] = React.useState(false);

  if (msg.role === "user") {
    const wide = hasMermaidFence(msg.content);
    const collapsible = shouldCollapseUserMessage(msg.content);
    const collapsed = collapsible && !userMessageExpanded;
    const bubbleWidth = wide ? "w-full max-w-[92%]" : "max-w-[92%] sm:max-w-[78%]";
    const canEdit = Boolean(historyMutable && onEditUser && parseMessageId(msg.id));
    const queued = msg.delivery_state === "queued";
    const attachments = msg.attachments ?? [];
    const bubbleTone = queued
      ? "bg-zinc-300 text-zinc-800"
      : "bg-zinc-900 text-zinc-50";
    const collapseButtonTone = queued
      ? "text-zinc-600 hover:bg-zinc-400/40 hover:text-zinc-900"
      : "text-zinc-300 hover:bg-zinc-800 hover:text-zinc-50";
    return (
      <div className="group flex justify-end">
        <div
          className={`${bubbleWidth} relative rounded-2xl px-4 py-2 text-sm ${bubbleTone}`}
        >
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

          {collapsible ? (
            <button
              type="button"
              className={`absolute right-2 top-2 rounded-md p-1 ${collapseButtonTone}`}
              aria-label={userMessageExpanded ? "Collapse message" : "Expand message"}
              title={userMessageExpanded ? "Collapse" : "Expand"}
              aria-expanded={userMessageExpanded}
              onClick={() => setUserMessageExpanded((v) => !v)}
            >
              {userMessageExpanded ? (
                <ChevronUp className="h-4 w-4" />
              ) : (
                <ChevronDown className="h-4 w-4" />
              )}
            </button>
          ) : null}

          {msg.content ? (
            <div
              className={[
                "whitespace-pre-wrap break-words",
                collapsible ? "pr-7" : "",
                collapsed ? "max-h-32 overflow-hidden" : "",
              ].join(" ")}
            >
              {msg.content}
            </div>
          ) : null}
          {renderUserAttachments(attachments, !queued, Boolean(msg.content))}
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
