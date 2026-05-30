export function stripWindowsNamespace(path: string): string {
  if (path.startsWith("\\\\?\\UNC\\")) return `\\\\${path.slice("\\\\?\\UNC\\".length)}`;
  if (path.startsWith("\\\\?\\")) return path.slice("\\\\?\\".length);
  return path;
}

export function pathString(value: unknown): string {
  if (typeof value === "string") return stripWindowsNamespace(value);
  if (value == null) return "";
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (typeof value === "object") {
    const path = (value as { path?: unknown }).path;
    if (typeof path === "string") return stripWindowsNamespace(path);
  }
  return "";
}

function normalizePathForMatch(p: unknown): string {
  return pathString(p).replaceAll("\\", "/");
}

export function workspaceBasename(root: unknown): string {
  const normalized = normalizePathForMatch(root).replace(/\/+$/, "");
  if (!normalized) return pathString(root);
  if (normalized === "/") return "/";
  const idx = normalized.lastIndexOf("/");
  const base = idx === -1 ? normalized : normalized.slice(idx + 1);
  return base || normalized;
}

export function isTmpWorkspaceRoot(root: unknown): boolean {
  const p = normalizePathForMatch(root).toLowerCase();
  return p.includes("/.kiliax/workspace/tmp_");
}

function middleEllipsis(text: unknown, headChars: number, tailChars: number): string {
  const s = pathString(text);
  if (s.length <= headChars + tailChars + 1) return s;
  return `${s.slice(0, Math.max(0, headChars))}...${s.slice(Math.max(0, s.length - tailChars))}`;
}

export function workspaceDisplayName(root: unknown): string {
  const base = workspaceBasename(root);
  if (!base) return base;
  if (!isTmpWorkspaceRoot(root)) return base;
  return middleEllipsis(base, 22, 6);
}
