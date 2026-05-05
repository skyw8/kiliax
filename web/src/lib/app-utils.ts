import { useEffect, useState } from "react";

export function splitModelId(
  modelId: string,
): { provider: string | null; model: string } {
  const raw = (modelId ?? "").trim();
  const idx = raw.indexOf("/");
  if (idx === -1) return { provider: null, model: raw };
  return { provider: raw.slice(0, idx), model: raw.slice(idx + 1) };
}

export function modelLabel(modelId: string): string {
  const { provider, model } = splitModelId(modelId);
  if (!provider) return model;
  return `${model} (${provider})`;
}

export function hasMermaidFence(text?: string | null): boolean {
  return /(^|\n)```[ \t]*mermaid\b/i.test(text ?? "");
}

export function stringifyUnknown(v: unknown): string {
  if (v == null) return "";
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}

export function parseMessageId(id: string): bigint | null {
  const trimmed = (id ?? "").trim();
  if (!/^\d+$/.test(trimmed)) return null;
  try {
    const v = BigInt(trimmed);
    if (v <= 0n) return null;
    return v;
  } catch {
    return null;
  }
}

export function messageIdToSafeNumber(id: bigint): number {
  if (id > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error("Message id is too large");
  }
  return Number(id);
}

export function newAlertId(prefix: string): string {
  return `${prefix}_${Date.now().toString(16)}_${Math.random().toString(16).slice(2)}`;
}

export function monotonicNowMs(): number {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return performance.now();
  }
  return Date.now();
}

export function isOverlaySidebarViewport(): boolean {
  if (typeof window === "undefined") return false;
  return window.matchMedia?.("(max-width: 767px)")?.matches ?? false;
}

export function useOverlaySidebarViewport(): boolean {
  const [matches, setMatches] = useState<boolean>(() => isOverlaySidebarViewport());

  useEffect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia?.("(max-width: 767px)");
    if (!mq) return;

    const onChange = () => setMatches(mq.matches);
    onChange();

    if (typeof mq.addEventListener === "function") {
      mq.addEventListener("change", onChange);
      return () => mq.removeEventListener("change", onChange);
    }
    // Safari < 14
    // eslint-disable-next-line deprecation/deprecation
    mq.addListener(onChange);
    // eslint-disable-next-line deprecation/deprecation
    return () => mq.removeListener(onChange);
  }, []);

  return matches;
}

export function fmtDurationCompact(durationMs: number): string {
  const ms = Math.max(0, Math.round(durationMs));
  if (ms >= 60_000) {
    const sec = Math.floor(ms / 1000);
    const minutes = Math.floor(sec / 60);
    const seconds = sec % 60;
    return `${minutes}m${String(seconds).padStart(2, "0")}s`;
  }
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(1)}s`;
  return `${ms}ms`;
}

export function fmtTokenUsage(usage?: {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cached_tokens?: number | null;
} | null): string | null {
  if (!usage) return null;
  const parts = [
    `Tokens: in ${usage.prompt_tokens}`,
    `out ${usage.completion_tokens}`,
    `total ${usage.total_tokens}`,
  ];
  const cached = usage.cached_tokens ?? 0;
  if (cached > 0) {
    parts.push(`cached ${cached}`);
  }
  return parts.join(" · ");
}

export async function copyToClipboard(text: string): Promise<boolean> {
  const value = text ?? "";
  try {
    await navigator.clipboard.writeText(value);
    return true;
  } catch {
    try {
      const el = document.createElement("textarea");
      el.value = value;
      el.style.position = "fixed";
      el.style.left = "-9999px";
      el.style.top = "0";
      document.body.appendChild(el);
      el.focus();
      el.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(el);
      return ok;
    } catch {
      return false;
    }
  }
}
