// The typed view state of a query (profile §2.4). A failed request is
// always `unavailable`, never an empty result. The battery view adds a
// `stale_usable` state (design §7) when it needs one.
import { ApiRequestError } from 'telemetry-web-api-client';
import type { ApiError } from 'telemetry-web-api-client';

export type ViewState<T> =
  | { readonly kind: 'loading' }
  | { readonly kind: 'unavailable'; readonly error: ApiError | null }
  | { readonly kind: 'data'; readonly data: T };

/** The part of a TanStack Query result that the view state reads. */
export type QuerySnapshot<T> = {
  readonly status: 'pending' | 'error' | 'success';
  readonly data: T | undefined;
  readonly error: unknown;
};

export function toViewState<T>(query: QuerySnapshot<T>): ViewState<T> {
  switch (query.status) {
    case 'pending':
      return { kind: 'loading' };
    case 'error':
      // Earlier data stays in the cache after a failed refresh. This view
      // shows the failure, not the old data.
      return {
        kind: 'unavailable',
        error: query.error instanceof ApiRequestError ? query.error.error : null,
      };
    case 'success':
      return query.data === undefined ? { kind: 'unavailable', error: null } : { kind: 'data', data: query.data };
    default:
      return unknownStatus(query.status);
  }
}

function unknownStatus(_status: never): ViewState<never> {
  return { kind: 'unavailable', error: null };
}
