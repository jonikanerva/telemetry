// The version of the SPA and the version of the server.
//
// The server writes its version into the `telemetry-version` meta element
// of index.html and returns the same value from /health. The value is a
// git commit SHA (7 to 40 lowercase hexadecimal characters) or "unknown".
//
// Rule: a value that is not such a SHA is unknown. This covers "unknown",
// the unreplaced placeholder of the development server, an empty value, and
// a missing meta element. The reload banner shows only when both versions
// are known and differ. An unknown version on either side gives no banner,
// because the SPA cannot tell an old tab from a new one.

export type Version = { readonly kind: 'known'; readonly sha: string } | { readonly kind: 'unknown' };

const SHA = /^[0-9a-f]{7,40}$/;

export function parseVersion(raw: string | null | undefined): Version {
  if (raw !== null && raw !== undefined && SHA.test(raw)) {
    return { kind: 'known', sha: raw };
  }
  return { kind: 'unknown' };
}

export function needsReload(client: Version, server: Version): boolean {
  return client.kind === 'known' && server.kind === 'known' && client.sha !== server.sha;
}

/** Reads the version that the server wrote into index.html. */
export function readClientVersion(document: Document): Version {
  const meta = document.querySelector('meta[name="telemetry-version"]');
  return parseVersion(meta?.getAttribute('content'));
}
