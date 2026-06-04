const TOKEN_KEY = "kiliax_token";

export function getAuthToken(): string {
  return sessionStorage.getItem(TOKEN_KEY)?.trim() ?? "";
}

export function setAuthToken(token: string): void {
  const value = token.trim();
  if (value) sessionStorage.setItem(TOKEN_KEY, value);
  else sessionStorage.removeItem(TOKEN_KEY);
}

export function clearAuthToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
}

export function bootstrapAuthTokenFromUrl(): string {
  const url = new URL(window.location.href);
  const token = url.searchParams.get("token")?.trim() ?? "";
  if (token) setAuthToken(token);
  if (url.searchParams.has("token")) {
    url.searchParams.delete("token");
    window.history.replaceState({}, "", url.pathname + url.search + url.hash);
  }
  return getAuthToken();
}
