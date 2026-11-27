import { ApiRequestError } from 'telemetry-web-api-client';
import type { ApiError } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import { createQueryClient, shouldRetry } from './query-client.js';

const busy: ApiError = {
  kind: 'problem',
  problem: { type: 'about:blank', title: 'x', status: 503, code: 'service_busy', detail: 'x' },
};

describe('shouldRetry', () => {
  it('retries a retryable failure once', () => {
    expect(shouldRetry(0, new ApiRequestError(busy))).toBe(true);
    expect(shouldRetry(1, new ApiRequestError(busy))).toBe(false);
    expect(shouldRetry(0, new ApiRequestError({ kind: 'network' }))).toBe(true);
  });

  it('does not retry other failures', () => {
    expect(shouldRetry(0, new ApiRequestError({ kind: 'timeout' }))).toBe(false);
    expect(shouldRetry(0, new ApiRequestError({ kind: 'invalid_response', status: 502 }))).toBe(false);
    expect(shouldRetry(0, new Error('defect'))).toBe(false);
  });
});

describe('createQueryClient', () => {
  it('turns off all background refetches', () => {
    const queries = createQueryClient().getDefaultOptions().queries;
    expect(queries?.refetchOnWindowFocus).toBe(false);
    expect(queries?.refetchOnReconnect).toBe(false);
    expect(queries?.refetchInterval).toBe(false);
    expect(queries?.staleTime).toBe(300_000);
    expect(queries?.gcTime).toBe(1_800_000);
  });
});
