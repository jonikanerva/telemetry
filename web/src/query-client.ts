// The TanStack Query client and its explicit defaults (design §7).
// No background traffic: no refetch on focus, on reconnect, or on an
// interval. The owner refreshes with the "Päivitä" button.
import { QueryClient } from '@tanstack/react-query';
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

export function createQueryClient(): QueryClient {
  return new QueryClient({
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
