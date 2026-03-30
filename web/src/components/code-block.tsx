import React, { useMemo } from "react";
import { cn } from "@/lib/utils";

type TokenKind =
  | "plain"
  | "comment"
  | "string"
  | "number"
  | "keyword"
  | "property"
  | "literal"
  | "punct";

type Token = { kind: TokenKind; value: string };

function normalizeLang(lang?: string | null): string | null {
  const v = (lang ?? "").trim().toLowerCase();
  if (!v) return null;
  if (v === "rs") return "rust";
  if (v === "js") return "javascript";
  if (v === "ts") return "typescript";
  if (v === "sh" || v === "shell") return "bash";
  if (v === "yml") return "yaml";
  if (v === "json5" || v === "jsonc") return "json";
  return v;
}

function safeJsonParse(raw: string): any | null {
  const t = raw.trim();
  if (!t) return null;
  try {
    return JSON.parse(t);
  } catch {
    return null;
  }
}

function pushToken(tokens: Token[], kind: TokenKind, value: string) {
  if (!value) return;
  const last = tokens[tokens.length - 1];
  if (last && last.kind === kind) {
    last.value += value;
    return;
  }
  tokens.push({ kind, value });
}

function jsonTokens(value: any, indent = 0): Token[] {
  const tokens: Token[] = [];
  const pad = (n: number) => "  ".repeat(n);

  const walk = (v: any, level: number) => {
    if (v === null) {
      pushToken(tokens, "literal", "null");
      return;
    }
    if (typeof v === "boolean") {
      pushToken(tokens, "literal", v ? "true" : "false");
      return;
    }
    if (typeof v === "number") {
      pushToken(tokens, "number", String(v));
      return;
    }
    if (typeof v === "string") {
      pushToken(tokens, "string", JSON.stringify(v));
      return;
    }
    if (Array.isArray(v)) {
      pushToken(tokens, "punct", "[");
      if (v.length === 0) {
        pushToken(tokens, "punct", "]");
        return;
      }
      pushToken(tokens, "plain", "\n");
      for (let i = 0; i < v.length; i += 1) {
        pushToken(tokens, "plain", pad(level + 1));
        walk(v[i], level + 1);
        if (i < v.length - 1) pushToken(tokens, "punct", ",");
        pushToken(tokens, "plain", "\n");
      }
      pushToken(tokens, "plain", pad(level));
      pushToken(tokens, "punct", "]");
      return;
    }
    if (typeof v === "object") {
      const entries = Object.entries(v as Record<string, any>);
      pushToken(tokens, "punct", "{");
      if (entries.length === 0) {
        pushToken(tokens, "punct", "}");
        return;
      }
      pushToken(tokens, "plain", "\n");
      for (let i = 0; i < entries.length; i += 1) {
        const [k, val] = entries[i] ?? ["", null];
        pushToken(tokens, "plain", pad(level + 1));
        pushToken(tokens, "property", JSON.stringify(k));
        pushToken(tokens, "punct", ": ");
        walk(val, level + 1);
        if (i < entries.length - 1) pushToken(tokens, "punct", ",");
        pushToken(tokens, "plain", "\n");
      }
      pushToken(tokens, "plain", pad(level));
      pushToken(tokens, "punct", "}");
      return;
    }

    pushToken(tokens, "plain", JSON.stringify(v));
  };

  walk(value, indent);
  return tokens;
}

function keywordSet(lang: string | null): Set<string> {
  if (lang === "rust") {
    return new Set([
      "as",
      "async",
      "await",
      "break",
      "const",
      "continue",
      "crate",
      "dyn",
      "else",
      "enum",
      "extern",
      "false",
      "fn",
      "for",
      "if",
      "impl",
      "in",
      "let",
      "loop",
      "match",
      "mod",
      "move",
      "mut",
      "pub",
      "ref",
      "return",
      "self",
      "Self",
      "static",
      "struct",
      "super",
      "trait",
      "true",
      "type",
      "unsafe",
      "use",
      "where",
      "while",
    ]);
  }
  if (lang === "typescript" || lang === "tsx" || lang === "javascript" || lang === "jsx") {
    return new Set([
      "as",
      "async",
      "await",
      "break",
      "case",
      "catch",
      "class",
      "const",
      "continue",
      "debugger",
      "default",
      "delete",
      "do",
      "else",
      "enum",
      "export",
      "extends",
      "false",
      "finally",
      "for",
      "from",
      "function",
      "get",
      "if",
      "implements",
      "import",
      "in",
      "instanceof",
      "interface",
      "let",
      "new",
      "null",
      "of",
      "private",
      "protected",
      "public",
      "readonly",
      "return",
      "set",
      "static",
      "super",
      "switch",
      "this",
      "throw",
      "true",
      "try",
      "type",
      "typeof",
      "undefined",
      "var",
      "void",
      "while",
      "with",
      "yield",
    ]);
  }
  if (lang === "python") {
    return new Set([
      "and",
      "as",
      "assert",
      "async",
      "await",
      "break",
      "case",
      "class",
      "continue",
      "def",
      "del",
      "elif",
      "else",
      "except",
      "false",
      "finally",
      "for",
      "from",
      "global",
      "if",
      "import",
      "in",
      "is",
      "lambda",
      "match",
      "none",
      "nonlocal",
      "not",
      "or",
      "pass",
      "raise",
      "return",
      "true",
      "try",
      "while",
      "with",
      "yield",
    ]);
  }
  if (lang === "bash") {
    return new Set([
      "case",
      "do",
      "done",
      "elif",
      "else",
      "esac",
      "fi",
      "for",
      "function",
      "if",
      "in",
      "select",
      "then",
      "time",
      "until",
      "while",
    ]);
  }
  return new Set();
}

function tokenizeCode(code: string, lang: string | null): Token[] {
  const tokens: Token[] = [];
  const keywords = keywordSet(lang);
  const supportsSlashComments =
    lang === "rust" ||
    lang === "typescript" ||
    lang === "tsx" ||
    lang === "javascript" ||
    lang === "jsx" ||
    lang === "go" ||
    lang === "c" ||
    lang === "cpp" ||
    lang === "java";
  const supportsHashComments =
    lang === "bash" || lang === "python" || lang === "yaml" || lang === "toml";
  const supportsBackticks = lang === "typescript" || lang === "tsx" || lang === "javascript" || lang === "jsx";

  const isIdentStart = (ch: string) => /[A-Za-z_]/.test(ch);
  const isIdentPart = (ch: string) => /[A-Za-z0-9_]/.test(ch);
  const isDigit = (ch: string) => /[0-9]/.test(ch);

  let i = 0;
  while (i < code.length) {
    const ch = code[i] ?? "";
    const next = code[i + 1] ?? "";

    if (supportsSlashComments && ch === "/" && next === "/") {
      let j = i + 2;
      while (j < code.length && (code[j] ?? "") !== "\n") j += 1;
      pushToken(tokens, "comment", code.slice(i, j));
      i = j;
      continue;
    }

    if (supportsHashComments && ch === "#") {
      let j = i + 1;
      while (j < code.length && (code[j] ?? "") !== "\n") j += 1;
      pushToken(tokens, "comment", code.slice(i, j));
      i = j;
      continue;
    }

    if (supportsSlashComments && ch === "/" && next === "*") {
      let j = i + 2;
      while (j < code.length) {
        if ((code[j] ?? "") === "*" && (code[j + 1] ?? "") === "/") {
          j += 2;
          break;
        }
        j += 1;
      }
      pushToken(tokens, "comment", code.slice(i, j));
      i = j;
      continue;
    }

    if (ch === "\"" || ch === "'" || (supportsBackticks && ch === "`")) {
      const quote = ch;
      let j = i + 1;
      while (j < code.length) {
        const c = code[j] ?? "";
        if (c === "\\") {
          j += 2;
          continue;
        }
        if (c === quote) {
          j += 1;
          break;
        }
        j += 1;
      }
      pushToken(tokens, "string", code.slice(i, j));
      i = j;
      continue;
    }

    if (isDigit(ch)) {
      let j = i + 1;
      while (j < code.length) {
        const c = code[j] ?? "";
        if (/[0-9a-fA-FxobXOB._]/.test(c)) {
          j += 1;
          continue;
        }
        break;
      }
      pushToken(tokens, "number", code.slice(i, j));
      i = j;
      continue;
    }

    if (isIdentStart(ch)) {
      let j = i + 1;
      while (j < code.length && isIdentPart(code[j] ?? "")) j += 1;
      const ident = code.slice(i, j);
      if (keywords.has(ident)) pushToken(tokens, "keyword", ident);
      else pushToken(tokens, "plain", ident);
      i = j;
      continue;
    }

    pushToken(tokens, "plain", ch);
    i += 1;
  }

  return tokens;
}

function tokenClass(kind: TokenKind): string {
  switch (kind) {
    case "comment":
      return "text-[#6A9955]";
    case "string":
      return "text-[#CE9178]";
    case "number":
      return "text-[#B5CEA8]";
    case "keyword":
      return "text-[#C586C0]";
    case "property":
      return "text-[#9CDCFE]";
    case "literal":
      return "text-[#569CD6]";
    case "punct":
    case "plain":
    default:
      return "text-[#D4D4D4]";
  }
}

export function CodeBlock({
  code,
  lang,
  className,
}: {
  code: string;
  lang?: string | null;
  className?: string;
}) {
  const normalizedLang = normalizeLang(lang);
  const content = useMemo(() => code.replace(/\r\n/g, "\n"), [code]);

  const tokens = useMemo(() => {
    if (normalizedLang === "json") {
      const parsed = safeJsonParse(content);
      if (parsed != null) return jsonTokens(parsed);
    }
    return tokenizeCode(content, normalizedLang);
  }, [content, normalizedLang]);

  return (
    <pre
      className={cn(
        "overflow-auto rounded-md bg-[#1e1e1e] p-3 text-xs leading-relaxed",
        className,
      )}
    >
      <code className="font-mono">
        {tokens.map((t, idx) => (
          <span key={idx} className={tokenClass(t.kind)}>
            {t.value}
          </span>
        ))}
      </code>
    </pre>
  );
}

