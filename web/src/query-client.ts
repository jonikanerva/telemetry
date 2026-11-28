// The TanStack Query client and its explicit defaults.
// No background traffic: no refetch on focus, on reconnect, or on an
// interval. The owner refreshes with the "Päivitä" button.
import { MutationCache, QueryCache, QueryClient } from '@tanstack/react-query';
import { ApiRequestError, isRetryable } from 'telemetry-web-api-client';

export const STALE_TIME_MS = 5 * 60 * 1000;
export const GC_TIME_MS = 30 * 60 * 1000;
export const RETRY_DELAY_MS = 2000;

/**
 * Retry a read once, only when the server was busy or unavailable or the
 * network failed. `failureCount` is the number of earlier failures.
 */
export function shouldRetry(failureCount: number, error: unknown): boolean {
  return failureCount < 1 && error instanceof ApiRequestError && isRetryable(error.error);
}

/**
 * A function that sees the error of every failed query and mutation, with
 * the `meta` of the failed query or mutation. The session-loss handler
 * (auth/session-loss.ts) uses it.
 */
export type CacheErrorHandler = (error: unknown, meta: Readonly<Record<string, unknown>> | undefined) => void;

export type QueryClientOptions = {
  readonly onError?: CacheErrorHandler;
};

export function createQueryClient(options: QueryClientOptions = {}): QueryClient {
  const onError = options.onError ?? ignoreError;
  return new QueryClient({
    queryCache: new QueryCache({
      onError: (error, query) => {
        onError(error, query.meta);
      },
    }),
    mutationCache: new MutationCache({
      onError: (error, _variables, _onMutateResult, mutation) => {
        onError(error, mutation.meta);
      },
    }),
    defaultOptions: {
      queries: {
        staleTime: STALE_TIME_MS,
        gcTime: GC_TIME_MS,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        refetchInterval: false,
        retry: shouldRetry,
        retryDelay: RETRY_DELAY_MS,
      },
      mutations: {
        retry: false,
      },
    },
  });
}

function ignoreError(): void {
  // No global error handling without a handler.
}
