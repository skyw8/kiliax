import React, { useMemo } from "react";
import { cn } from "@/lib/utils";
import { CodeBlock } from "@/components/code-block";

type InlineToken =
  | { type: "text"; value: string }
  | { type: "code"; value: string }
  | { type: "bold"; children: InlineToken[] }
  | { type: "italic"; children: InlineToken[] }
  | { type: "strike"; children: InlineToken[] }
  | { type: "link"; href: string; children: InlineToken[] };

type TableAlign = "left" | "center" | "right" | null;

type Block =
  | { type: "paragraph"; text: string }
  | { type: "heading"; level: number; text: string }
  | { type: "ul"; items: string[] }
  | { type: "ol"; items: string[] }
  | { type: "blockquote"; text: string }
  | { type: "hr" }
  | { type: "code"; lang: string | null; code: string }
  | { type: "table"; header: string[]; align: TableAlign[]; rows: string[][] };

function safeHref(raw: string): string | null {
  const href = raw.trim();
  if (!href) return null;
  const lower = href.toLowerCase();
  if (lower.startsWith("http://")) return href;
  if (lower.startsWith("https://")) return href;
  if (lower.startsWith("mailto:")) return href;
  if (href.startsWith("/") || href.startsWith("#")) return href;
  return null;
}

function tokenizeInline(input: string): InlineToken[] {
  const tokens: InlineToken[] = [];
  let i = 0;
  let buf = "";

  const flush = () => {
    if (!buf) return;
    tokens.push({ type: "text", value: buf });
    buf = "";
  };

  while (i < input.length) {
    // code span
    if (input[i] === "`") {
      const end = input.indexOf("`", i + 1);
      if (end !== -1) {
        flush();
        tokens.push({ type: "code", value: input.slice(i + 1, end) });
        i = end + 1;
        continue;
      }
    }

    // bold
    if (input.startsWith("**", i)) {
      const end = input.indexOf("**", i + 2);
      if (end !== -1) {
        flush();
        const inner = input.slice(i + 2, end);
        tokens.push({ type: "bold", children: tokenizeInline(inner) });
        i = end + 2;
        continue;
      }
    }

    // strikethrough
    if (input.startsWith("~~", i)) {
      const end = input.indexOf("~~", i + 2);
      if (end !== -1) {
        flush();
        const inner = input.slice(i + 2, end);
        tokens.push({ type: "strike", children: tokenizeInline(inner) });
        i = end + 2;
        continue;
      }
    }

    // link: [text](url)
    if (input[i] === "[") {
      const closeBracket = input.indexOf("]", i + 1);
      if (closeBracket !== -1 && input[closeBracket + 1] === "(") {
        const closeParen = input.indexOf(")", closeBracket + 2);
        if (closeParen !== -1) {
          const label = input.slice(i + 1, closeBracket);
          const href = input.slice(closeBracket + 2, closeParen);
          flush();
          tokens.push({
            type: "link",
            href,
            children: tokenizeInline(label),
          });
          i = closeParen + 1;
          continue;
        }
      }
    }

    // italic (*text*)
    if (input[i] === "*") {
      const end = input.indexOf("*", i + 1);
      if (end !== -1) {
        flush();
        const inner = input.slice(i + 1, end);
        tokens.push({ type: "italic", children: tokenizeInline(inner) });
        i = end + 1;
        continue;
      }
    }

    buf += input[i];
    i += 1;
  }

  flush();
  return tokens;
}

function splitTableRow(line: string): string[] | null {
  if (!line.includes("|")) return null;

  const cells: string[] = [];
  let buf = "";
  let inCode = false;

  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i] ?? "";

    if (ch === "`") {
      inCode = !inCode;
      buf += ch;
      continue;
    }

    if (ch === "\\" && i + 1 < line.length) {
      const next = line[i + 1] ?? "";
      if (next === "|" || next === "\\") {
        buf += next;
        i += 1;
        continue;
      }
    }

    if (ch === "|" && !inCode) {
      cells.push(buf.trim());
      buf = "";
      continue;
    }

    buf += ch;
  }

  cells.push(buf.trim());

  const trimmed = line.trim();
  let out = cells;
  if (trimmed.startsWith("|")) out = out.slice(1);
  if (trimmed.endsWith("|")) out = out.slice(0, -1);
  if (!out.length) return null;
  return out;
}

function parseTableAlign(cells: string[], columns: number): TableAlign[] | null {
  if (cells.length !== columns) return null;
  const re = /^:?-{3,}:?$/;
  const align: TableAlign[] = [];
  for (let i = 0; i < columns; i += 1) {
    const cell = (cells[i] ?? "").trim();
    if (!re.test(cell)) return null;
    const starts = cell.startsWith(":");
    const ends = cell.endsWith(":");
    if (starts && ends) align.push("center");
    else if (ends) align.push("right");
    else if (starts) align.push("left");
    else align.push(null);
  }
  return align;
}

function normalizeTableRow(cells: string[], columns: number): string[] {
  if (cells.length === columns) return cells;
  if (cells.length < columns) {
    return [...cells, ...Array.from({ length: columns - cells.length }, () => "")];
  }
  const head = cells.slice(0, columns - 1);
  const tail = cells.slice(columns - 1).join(" | ");
  return [...head, tail];
}

function parseTableBlock(
  lines: string[],
  startIndex: number,
): { block: Extract<Block, { type: "table" }>; nextIndex: number } | null {
  const headerCells = splitTableRow(lines[startIndex] ?? "");
  if (!headerCells) return null;

  const separatorLine = lines[startIndex + 1];
  if (separatorLine == null) return null;
  const separatorCells = splitTableRow(separatorLine);
  if (!separatorCells) return null;

  const columns = Math.max(headerCells.length, separatorCells.length);
  if (separatorCells.length !== columns) return null;

  const align = parseTableAlign(separatorCells, columns);
  if (!align) return null;

  const header = normalizeTableRow(headerCells, columns);
  const rows: string[][] = [];

  let i = startIndex + 2;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (!line.trim()) break;
    const rowCells = splitTableRow(line);
    if (!rowCells) break;
    rows.push(normalizeTableRow(rowCells, columns));
    i += 1;
  }

  return { block: { type: "table", header, align, rows }, nextIndex: i };
}

function parseMarkdown(input: string): Block[] {
  const lines = input.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];

  const isCodeFence = (line: string) => line.trimStart().startsWith("```");
  const isHeading = (line: string) => /^#{1,6}\s+/.test(line);
  const isHr = (line: string) => {
    const t = line.trim();
    return t === "---" || t === "***" || t === "___";
  };
  const isBlockquote = (line: string) => line.startsWith(">");
  const ulMatch = (line: string) => line.match(/^\s*[-*+]\s+(.*)$/);
  const olMatch = (line: string) => line.match(/^\s*\d+\.\s+(.*)$/);

  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";

    if (!line.trim()) {
      i += 1;
      continue;
    }

    if (isHr(line)) {
      blocks.push({ type: "hr" });
      i += 1;
      continue;
    }

    if (isCodeFence(line)) {
      const lang = line.trim().slice(3).trim() || null;
      i += 1;
      const codeLines: string[] = [];
      while (i < lines.length && !isCodeFence(lines[i] ?? "")) {
        codeLines.push(lines[i] ?? "");
        i += 1;
      }
      if (i < lines.length && isCodeFence(lines[i] ?? "")) {
        i += 1;
      }
      blocks.push({ type: "code", lang, code: codeLines.join("\n") });
      continue;
    }

    if (isHeading(line)) {
      const m = line.match(/^(#{1,6})\s+(.*)$/);
      blocks.push({ type: "heading", level: m ? m[1].length : 1, text: m ? m[2] : line });
      i += 1;
      continue;
    }

    if (isBlockquote(line)) {
      const quoteLines: string[] = [];
      while (i < lines.length && isBlockquote(lines[i] ?? "")) {
        quoteLines.push((lines[i] ?? "").replace(/^>\s?/, ""));
        i += 1;
      }
      blocks.push({ type: "blockquote", text: quoteLines.join("\n") });
      continue;
    }

    const table = parseTableBlock(lines, i);
    if (table) {
      blocks.push(table.block);
      i = table.nextIndex;
      continue;
    }

    const ul = ulMatch(line);
    if (ul) {
      const items: string[] = [];
      while (i < lines.length) {
        const m = ulMatch(lines[i] ?? "");
        if (!m) break;
        items.push(m[1] ?? "");
        i += 1;
      }
      blocks.push({ type: "ul", items });
      continue;
    }

    const ol = olMatch(line);
    if (ol) {
      const items: string[] = [];
      while (i < lines.length) {
        const m = olMatch(lines[i] ?? "");
        if (!m) break;
        items.push(m[1] ?? "");
        i += 1;
      }
      blocks.push({ type: "ol", items });
      continue;
    }

    const paraLines: string[] = [line];
    i += 1;
    while (i < lines.length) {
      const next = lines[i] ?? "";
      if (!next.trim()) break;
      if (isHr(next)) break;
      if (isCodeFence(next)) break;
      if (isHeading(next)) break;
      if (isBlockquote(next)) break;
      if (parseTableBlock(lines, i)) break;
      if (ulMatch(next) || olMatch(next)) break;
      paraLines.push(next);
      i += 1;
    }
    blocks.push({ type: "paragraph", text: paraLines.join("\n") });
  }

  return blocks;
}

function renderInlineTokens(
  tokens: InlineToken[],
  keyPrefix: string,
): React.ReactNode[] {
  return tokens.map((t, idx) => {
    const key = `${keyPrefix}:${idx}`;
    if (t.type === "text") return <React.Fragment key={key}>{t.value}</React.Fragment>;
    if (t.type === "code") {
      return (
        <code
          key={key}
          className="rounded bg-zinc-200/60 px-1 py-0.5 font-mono text-[0.85em] text-zinc-900"
        >
          {t.value}
        </code>
      );
    }
    if (t.type === "bold") {
      return (
        <strong key={key} className="font-semibold">
          {renderInlineTokens(t.children, key)}
        </strong>
      );
    }
    if (t.type === "italic") {
      return (
        <em key={key} className="italic">
          {renderInlineTokens(t.children, key)}
        </em>
      );
    }
    if (t.type === "strike") {
      return (
        <span key={key} className="line-through opacity-90">
          {renderInlineTokens(t.children, key)}
        </span>
      );
    }
    if (t.type === "link") {
      const href = safeHref(t.href);
      if (!href) {
        return <React.Fragment key={key}>{renderInlineTokens(t.children, key)}</React.Fragment>;
      }
      return (
        <a
          key={key}
          href={href}
          target="_blank"
          rel="noreferrer"
          className="text-blue-600 underline underline-offset-2 hover:text-blue-700"
        >
          {renderInlineTokens(t.children, key)}
        </a>
      );
    }
    return null;
  });
}

function renderInline(text: string, keyPrefix: string): React.ReactNode[] {
  const parts = text.split("\n");
  const out: React.ReactNode[] = [];
  for (let i = 0; i < parts.length; i += 1) {
    if (i > 0) {
      out.push(<br key={`${keyPrefix}:br:${i}`} />);
    }
    const tokens = tokenizeInline(parts[i] ?? "");
    out.push(...renderInlineTokens(tokens, `${keyPrefix}:line:${i}`));
  }
  return out;
}

function renderBlock(block: Block, key: string): React.ReactNode {
  if (block.type === "hr") {
    return <div key={key} className="my-2 h-px w-full bg-zinc-200" />;
  }

  if (block.type === "code") {
    return (
      <CodeBlock key={key} code={block.code} lang={block.lang} />
    );
  }

  if (block.type === "table") {
    const alignClass = (a: TableAlign) => {
      if (a === "center") return "text-center";
      if (a === "right") return "text-right";
      return "text-left";
    };

    return (
      <div key={key} className="overflow-x-auto">
        <table className="w-full border-collapse text-sm">
          <thead className="bg-zinc-50 text-zinc-900">
            <tr>
              {block.header.map((cell, idx) => (
                <th
                  key={`${key}:th:${idx}`}
                  className={cn(
                    "border border-zinc-200 px-2 py-1 font-semibold",
                    alignClass(block.align[idx] ?? null),
                  )}
                >
                  {renderInline(cell, `${key}:th:${idx}`)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {block.rows.map((row, rowIdx) => (
              <tr key={`${key}:tr:${rowIdx}`} className="even:bg-zinc-50/40">
                {row.map((cell, colIdx) => (
                  <td
                    key={`${key}:td:${rowIdx}:${colIdx}`}
                    className={cn(
                      "border border-zinc-200 px-2 py-1 align-top",
                      alignClass(block.align[colIdx] ?? null),
                    )}
                  >
                    {renderInline(cell, `${key}:td:${rowIdx}:${colIdx}`)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    );
  }

  if (block.type === "heading") {
    const level = Math.min(6, Math.max(1, block.level));
    const Tag = `h${level}` as React.ElementType;
    const cls = [
      "font-semibold text-zinc-900",
      level <= 2 ? "text-sm" : "text-xs",
    ].join(" ");
    return (
      <Tag key={key} className={cls}>
        {renderInline(block.text, `${key}:h`)}
      </Tag>
    );
  }

  if (block.type === "blockquote") {
    return (
      <blockquote
        key={key}
        className="rounded-md bg-zinc-100 px-3 py-2 text-sm italic text-zinc-700"
      >
        {renderInline(block.text, `${key}:bq`)}
      </blockquote>
    );
  }

  if (block.type === "ul") {
    return (
      <ul key={key} className="ml-5 list-disc space-y-1 text-sm">
        {block.items.map((it, idx) => (
          <li key={`${key}:li:${idx}`}>{renderInline(it, `${key}:li:${idx}`)}</li>
        ))}
      </ul>
    );
  }

  if (block.type === "ol") {
    return (
      <ol key={key} className="ml-5 list-decimal space-y-1 text-sm">
        {block.items.map((it, idx) => (
          <li key={`${key}:li:${idx}`}>{renderInline(it, `${key}:li:${idx}`)}</li>
        ))}
      </ol>
    );
  }

  if (block.type === "paragraph") {
    return (
      <p key={key} className="text-sm">
        {renderInline(block.text, `${key}:p`)}
      </p>
    );
  }

  return null;
}

export function Markdown({ text, className }: { text: string; className?: string }) {
  const blocks = useMemo(() => parseMarkdown(text), [text]);
  return (
    <div className={cn("space-y-2 leading-relaxed", className)}>
      {blocks.map((b, idx) => renderBlock(b, `b:${idx}`))}
    </div>
  );
}
