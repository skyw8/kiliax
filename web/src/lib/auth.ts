const TOKEN_KEY = "kiliax_token";
const COOKIE_MAX_AGE_SECONDS = 60 * 60 * 24 * 30;

export function getAuthToken(): string {
  return sessionStorage.getItem(TOKEN_KEY)?.trim() || getCookieToken();
}

export function setAuthToken(token: string): void {
  const value = token.trim();
  if (value) {
    sessionStorage.setItem(TOKEN_KEY, value);
    document.cookie = `${TOKEN_KEY}=${encodeURIComponent(value)}; Path=/; SameSite=Lax; Max-Age=${COOKIE_MAX_AGE_SECONDS}`;
  } else {
    clearAuthToken();
  }
}

export function clearAuthToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
  document.cookie = `${TOKEN_KEY}=; Path=/; SameSite=Lax; Max-Age=0`;
}

export function bootstrapAuthTokenFromUrl(): string {
  const url = new URL(window.location.href);
  const token = url.searchParams.get("token")?.trim() ?? "";
  if (token) setAuthToken(token);
  else {
    const cookieToken = getCookieToken();
    if (cookieToken) setAuthToken(cookieToken);
  }
  if (url.searchParams.has("token")) {
    url.searchParams.delete("token");
    window.history.replaceState({}, "", url.pathname + url.search + url.hash);
  }
  return getAuthToken();
}

function getCookieToken(): string {
  const prefix = `${TOKEN_KEY}=`;
  for (const part of document.cookie.split(";")) {
    const value = part.trim();
    if (!value.startsWith(prefix)) continue;
    const raw = value.slice(prefix.length);
    try {
      return decodeURIComponent(raw).trim();
    } catch {
      return raw.trim();
    }
  }
  return "";
}
