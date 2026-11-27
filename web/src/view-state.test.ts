import { ApiRequestError } from 'telemetry-web-api-client';
import { describe, expect, it } from 'vitest';

import { toViewState } from './view-state.js';

describe('toViewState', () => {
  it('is loading while the first request runs', () => {
    expect(toViewState({ status: 'pending', data: undefined, error: null })).toEqual({ kind: 'loading' });
  });

  it('gives the data of a successful request', () => {
    expect(toViewState({ status: 'success', data: { version: 'x' }, error: null })).toEqual({
      kind: 'data',
      data: { version: 'x' },
    });
  });

  it.each([
    { kind: 'network' } as const,
    { kind: 'timeout' } as const,
    { kind: 'invalid_response', status: 502 } as const,
    {
      kind: 'problem',
      problem: { type: 'about:blank', title: 'x', status: 503, code: 'service_busy', detail: 'x' },
    } as const,
  ])('is unavailable for the API error $kind', (error) => {
    expect(toViewState({ status: 'error', data: undefined, error: new ApiRequestError(error) })).toEqual({
      kind: 'unavailable',
      error,
    });
  });

  it('is unavailable for an unexpected failure', () => {
    expect(toViewState({ status: 'error', data: undefined, error: new Error('defect') })).toEqual({
      kind: 'unavailable',
      error: null,
    });
  });

  it('shows the failure, not the old data, when a refresh fails', () => {
    const state = toViewState({
      status: 'error',
      data: { version: 'old' },
      error: new ApiRequestError({ kind: 'network' }),
    });
    expect(state).toEqual({ kind: 'unavailable', error: { kind: 'network' } });
  });

  it('never turns a success without data into an empty result', () => {
    expect(toViewState({ status: 'success', data: undefined, error: null })).toEqual({
      kind: 'unavailable',
      error: null,
    });
  });
});
