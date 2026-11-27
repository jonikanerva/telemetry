// The `pageshow` rule. After a navigation to another document, a browser
// can keep the old page in its back-forward cache and restore it from
// memory, with the protected data of a session that has since ended. The
// restore runs no route check. A reload makes the protected route ask
// the server for the session again.
//
// The server sends index.html with `Cache-Control: no-store`, which some
// engines already treat as a reason not to keep the page; this handler
// covers the other engines.
export function createPageShowHandler(reload: () => void): (event: Pick<PageTransitionEvent, 'persisted'>) => void {
  return (event) => {
    if (event.persisted) {
      reload();
    }
  };
}
