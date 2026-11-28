// The unavailable state of a route whose `beforeLoad` failed, for example
// when the session check got no valid answer. It never shows the error
// text, and it never looks like an empty result. The retry button runs
// the route checks again. When the session read got 400
// `invalid_request`, the browser holds two session cookies, and the page
// says how to fix that (auth/conflicting-cookies.ts).
import { useRouter } from '@tanstack/react-router';

import { fi } from '../i18n/fi.js';
import { isConflictingCookies } from './conflicting-cookies.js';
import { RouteUnavailableView } from './RouteUnavailableView.js';

export function RouteUnavailable({ error }: { readonly error?: unknown }) {
  const router = useRouter();
  return (
    <RouteUnavailableView
      body={isConflictingCookies(error) ? fi.conflictingCookies : fi.unavailableBody}
      onRetry={() => {
        void router.invalidate();
      }}
    />
  );
}
