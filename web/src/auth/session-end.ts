// The display text of the end of a session. The instant comes from the
// validated session body (RFC 3339 UTC with `Z`). The display zone is
// always Europe/Helsinki, never the zone of the browser.
const SESSION_END_FORMAT = new Intl.DateTimeFormat('fi-FI', {
  timeZone: 'Europe/Helsinki',
  dateStyle: 'short',
  timeStyle: 'short',
});

/** Formats an instant for Finnish display, or returns null for an invalid instant. */
export function formatSessionEnd(instant: string): string | null {
  const milliseconds = Date.parse(instant);
  return Number.isNaN(milliseconds) ? null : SESSION_END_FORMAT.format(new Date(milliseconds));
}
