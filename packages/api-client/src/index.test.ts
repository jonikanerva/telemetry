import { describe, expect, it } from 'vitest';
import { config as zodConfig } from 'zod';

import { createApiClient, isRetryable } from './index.js';
import type { ApiError } from './index.js';

const BASE_URL = 'http://api.test';

type Stub = (request: Request) => Promise<Response>;

function clientWith(stub: Stub, timeoutMs = 1_000) {
  const requests: Request[] = [];
  const fetchStub = async (input: RequestInfo | URL): Promise<Response> => {
    if (!(input instanceof Request)) {
      throw new TypeError('the generated client passes a Request');
    }
    requests.push(input);
    return stub(input);
  };
  const client = createApiClient({ baseUrl: BASE_URL, fetch: fetchStub, timeoutMs });
  return { client, requests };
}

function json(status: number, body: unknown, contentType = 'application/json'): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': contentType } });
}

function problem(status: number, code: string, extra: Record<string, unknown> = {}): Response {
  return json(
    status,
    { type: 'about:blank', title: 'Reason', status, code, detail: 'Static text.', ...extra },
    'application/problem+json',
  );
}

describe('getHealth', () => {
  it('returns a valid health body', async () => {
    const { client, requests } = clientWith(() => Promise.resolve(json(200, { status: 'ok', version: 'abc1234' })));
    const result = await client.getHealth();
    expect(result).toEqual({ ok: true, data: { status: 'ok', version: 'abc1234' } });
    expect(requests).toHaveLength(1);
    expect(requests[0]?.url).toBe(`${BASE_URL}/health`);
    expect(requests[0]?.method).toBe('GET');
    expect(requests[0]?.credentials).toBe('same-origin');
  });

  it('removes unknown keys from a valid success body', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(json(200, { status: 'ok', version: 'abc1234', extra: 'not in the contract' })),
    );
    expect(await client.getHealth()).toEqual({ ok: true, data: { status: 'ok', version: 'abc1234' } });
  });

  it('rejects a success body that does not match the schema', async () => {
    const { client } = clientWith(() => Promise.resolve(json(200, { status: 'ok' })));
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 200 },
    });
  });

  it('rejects an unknown enum value in a success body', async () => {
    const { client } = clientWith(() => Promise.resolve(json(200, { status: 'degraded', version: 'x' })));
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 200 },
    });
  });

  // With content-type detection, an HTML 200 page skips the validator. The
  // facade parses every body as JSON, so the page is an invalid response.
  it('rejects a 200 HTML page instead of returning it as data', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(
        new Response('<!doctype html><title>x</title>', { status: 200, headers: { 'Content-Type': 'text/html' } }),
      ),
    );
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 200 },
    });
  });

  it('returns a valid problem body as a problem', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(problem(503, 'database_unavailable', { retry_after_seconds: 30 })),
    );
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: {
        kind: 'problem',
        problem: {
          type: 'about:blank',
          title: 'Reason',
          status: 503,
          code: 'database_unavailable',
          detail: 'Static text.',
          retry_after_seconds: 30,
        },
      },
    });
  });

  it('rejects a problem body with an unknown code', async () => {
    const { client } = clientWith(() => Promise.resolve(problem(404, 'gone_fishing')));
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 404 },
    });
  });

  it('rejects a problem body without a status', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(
        json(
          404,
          { type: 'about:blank', title: 'Not Found', code: 'not_found', detail: 'x' },
          'application/problem+json',
        ),
      ),
    );
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 404 },
    });
  });

  it('rejects a problem body whose status differs from the HTTP status', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(
        json(
          500,
          { type: 'about:blank', title: 'Not Found', status: 404, code: 'not_found', detail: 'x' },
          'application/problem+json',
        ),
      ),
    );
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 500 },
    });
  });

  it('treats a non-JSON 502 page of an intermediary as an invalid response', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(
        new Response('<html>Bad Gateway</html>', { status: 502, headers: { 'Content-Type': 'text/html' } }),
      ),
    );
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 502 },
    });
  });

  it('treats an empty 503 body as an invalid response', async () => {
    const { client } = clientWith(() => Promise.resolve(new Response('', { status: 503 })));
    expect(await client.getHealth()).toEqual({
      ok: false,
      error: { kind: 'invalid_response', status: 503 },
    });
  });

  it('returns network when no response arrives', async () => {
    const { client } = clientWith(() => Promise.reject(new TypeError('fetch failed')));
    expect(await client.getHealth()).toEqual({ ok: false, error: { kind: 'network' } });
  });

  it('returns timeout when the deadline passes', async () => {
    // The stub never answers: the facade must end the request by itself.
    const { client } = clientWith(() => new Promise<Response>(() => undefined), 20);
    expect(await client.getHealth()).toEqual({ ok: false, error: { kind: 'timeout' } });
  });

  it('throws the abort reason when the caller cancels', async () => {
    const controller = new AbortController();
    const { client } = clientWith(() => {
      controller.abort();
      return new Promise<Response>(() => undefined);
    });
    await expect(client.getHealth({ signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
  });
});

describe('isRetryable', () => {
  const cases: ReadonlyArray<readonly [ApiError, boolean]> = [
    [{ kind: 'network' }, true],
    [{ kind: 'timeout' }, false],
    [{ kind: 'invalid_response', status: 502 }, false],
  ];

  it.each(cases)('classifies %o', (error, expected) => {
    expect(isRetryable(error)).toBe(expected);
  });

  it.each([
    ['database_unavailable', true],
    ['service_busy', true],
    ['timeout', true],
    ['internal_error', false],
    ['unauthenticated', false],
    ['not_found', false],
  ] as const)('classifies the problem code %s', (code, expected) => {
    const error: ApiError = {
      kind: 'problem',
      problem: { type: 'about:blank', title: 'x', status: 503, code, detail: 'x' },
    };
    expect(isRetryable(error)).toBe(expected);
  });
});

describe('runtime configuration', () => {
  // The server CSP forbids eval; the Zod eval probe would report a violation.
  it('turns off the Zod code generation', () => {
    expect(zodConfig().jitless).toBe(true);
  });
});
