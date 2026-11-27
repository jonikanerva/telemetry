// The target of a navigation after a login. The value comes from the URL
// (`/login?redirect=...`), so anybody can choose it. Only an internal path
// of the application is accepted; every other value becomes `/`.
//
// Rule: the value is a string of at most MAX_REDIRECT_LENGTH characters
// with no whitespace, no control character, and no backslash anywhere
// (`new URL` removes a tab or a newline, and a browser reads `\` as `/`).
// It starts with exactly one `/`. It contains no encoded `/` or `\`
// (`%2F`, `%5C`). It resolves to the page origin. Its parsed path does not
// start with `//` and is not the login page. The result is the normalised
// path, query, and hash of the parsed URL, never the raw input.

export const HOME_PATH = '/';
export const LOGIN_PATH = '/login';
export const MAX_REDIRECT_LENGTH = 2048;

const WHITESPACE = /\s/u;
const ENCODED_SLASH = /%(2f|5c)/iu;

export function safeRedirect(raw: unknown, origin: string): string {
  if (typeof raw !== 'string' || raw.length === 0 || raw.length > MAX_REDIRECT_LENGTH) {
    return HOME_PATH;
  }
  if (hasForbiddenCharacter(raw) || ENCODED_SLASH.test(raw)) {
    return HOME_PATH;
  }
  if (!raw.startsWith('/') || raw.startsWith('//')) {
    return HOME_PATH;
  }
  let url: URL;
  try {
    url = new URL(raw, origin);
  } catch {
    return HOME_PATH;
  }
  // Dot segments can collapse to a leading `//` (`/.//evil.example`),
  // which a browser reads as another host.
  if (url.origin !== origin || url.pathname.startsWith('//') || isLoginPath(url.pathname)) {
    return HOME_PATH;
  }
  return url.pathname + url.search + url.hash;
}

// Whitespace (including Unicode separators), C0 and C1 controls, and `\`.
function hasForbiddenCharacter(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code <= 0x20 || (code >= 0x7f && code <= 0x9f) || character === '\\' || WHITESPACE.test(character)) {
      return true;
    }
  }
  return false;
}

function isLoginPath(pathname: string): boolean {
  return pathname === LOGIN_PATH || pathname.startsWith(`${LOGIN_PATH}/`);
}

/** The validated search of the login page. */
export type LoginSearch = { readonly redirect?: string };

/**
 * Validates the search of `/login`. A missing value stays missing; any
 * other value becomes a safe internal path.
 */
export function parseLoginSearch(search: Readonly<Record<string, unknown>>, origin: string): LoginSearch {
  const raw = search['redirect'];
  return raw === undefined ? {} : { redirect: safeRedirect(raw, origin) };
}
