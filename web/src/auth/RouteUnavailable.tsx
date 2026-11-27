// The unavailable state of a route whose `beforeLoad` failed, for example
// when the session check got no valid answer. It never shows the error
// text, and it never looks like an empty result. The retry button runs
// the route checks again.
import { useRouter } from '@tanstack/react-router';

import { fi } from '../i18n/fi.js';

export function RouteUnavailable() {
  const router = useRouter();
  return (
    // No `aria-labelledby`: with a named region, axe crashed the Firefox
    // content process of the browser tests (see auth/LoginFormView.tsx).
    <section>
      <div role="status" data-state="unavailable" className="notice notice-error">
        <h1 id="unavailable-title" className="notice-title">
          {fi.unavailableTitle}
        </h1>
        <p>{fi.unavailableBody}</p>
      </div>
      <button
        type="button"
        className="retry-button"
        onClick={() => {
          void router.invalidate();
        }}
      >
        {fi.retry}
      </button>
    </section>
  );
}
