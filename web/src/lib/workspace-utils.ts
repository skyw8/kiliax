function normalizePathForMatch(p: string): string {
  return (p ?? "").replaceAll("\\", "/");
}

export function workspaceBasename(root: string): string {
  const normalized = normalizePathForMatch(root).replace(/\/+$/, "");
  if (!normalized) return root;
  if (normalized === "/") return "/";
  const idx = normalized.lastIndexOf("/");
  const base = idx === -1 ? normalized : normalized.slice(idx + 1);
  return base || normalized;
}

export function isTmpWorkspaceRoot(root: string): boolean {
  const p = normalizePathForMatch(root).toLowerCase();
  return p.includes("/.kiliax/workspace/tmp_");
}

function middleEllipsis(text: string, headChars: number, tailChars: number): string {
  const s = text ?? "";
  if (s.length <= headChars + tailChars + 1) return s;
  return `${s.slice(0, Math.max(0, headChars))}…${s.slice(Math.max(0, s.length - tailChars))}`;
}

export function workspaceDisplayName(root: string): string {
  const base = workspaceBasename(root);
  if (!base) return base;
  if (!isTmpWorkspaceRoot(root)) return base;
  return middleEllipsis(base, 22, 6);
}

