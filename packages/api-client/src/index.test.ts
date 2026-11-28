import { describe, expect, it } from 'vitest';
import { config as zodConfig } from 'zod';

import { createApiClient, isRetryable, isUnauthenticated } from './index.js';
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

const SESSION = { expires_at: '2026-12-27T08:00:00Z', idle_expires_at: '2026-12-04T08:00:00.123456Z' };

describe('getSession', () => {
  it('returns a valid session body', async () => {
    const { client, requests } = clientWith(() => Promise.resolve(json(200, SESSION)));
    expect(await client.getSession()).toEqual({ ok: true, data: SESSION });
    expect(requests[0]?.url).toBe(`${BASE_URL}/api/v1/session`);
    expect(requests[0]?.method).toBe('GET');
    expect(requests[0]?.credentials).toBe('same-origin');
  });

  it('returns the problem unauthenticated for a 401', async () => {
    const { client } = clientWith(() => Promise.resolve(problem(401, 'unauthenticated')));
    const result = await client.getSession();
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toMatchObject({ kind: 'problem', problem: { code: 'unauthenticated', status: 401 } });
      expect(isUnauthenticated(result.error)).toBe(true);
    }
  });

  // The generated client returns `{}` without validation for these
  // responses. An empty body must never open a session.
  it.each([
    ['a 204 without a body', () => new Response(null, { status: 204 })],
    ['a 200 with Content-Length 0', () => new Response('', { status: 200, headers: { 'Content-Length': '0' } })],
    ['a 200 with an empty body', () => new Response('', { status: 200 })],
    ['a 200 with an empty object', () => json(200, {})],
  ])('rejects %s', async (_name, response) => {
    const { client } = clientWith(() => Promise.resolve(response()));
    const result = await client.getSession();
    expect(result).toMatchObject({ ok: false, error: { kind: 'invalid_response' } });
  });

  it.each([
    ['an instant without Z', { ...SESSION, expires_at: '2026-12-27T08:00:00+00:00' }],
    ['an instant with nanoseconds', { ...SESSION, expires_at: '2026-12-27T08:00:00.123456789Z' }],
    ['a missing field', { expires_at: SESSION.expires_at }],
  ])('rejects a session body with %s', async (_name, body) => {
    const { client } = clientWith(() => Promise.resolve(json(200, body)));
    expect(await client.getSession()).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });

  it('throws the abort reason when the caller cancels', async () => {
    const controller = new AbortController();
    const { client } = clientWith(() => {
      controller.abort();
      return new Promise<Response>(() => undefined);
    });
    await expect(client.getSession({ signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
  });
});

describe('getHealth with an empty success body', () => {
  it('rejects a 200 with Content-Length 0', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(new Response('', { status: 200, headers: { 'Content-Length': '0' } })),
    );
    expect(await client.getHealth()).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });
});

describe('login', () => {
  it('posts the password as JSON and returns the session', async () => {
    const { client, requests } = clientWith(() => Promise.resolve(json(200, SESSION)));
    expect(await client.login('correct horse')).toEqual({ ok: true, data: SESSION });
    const request = requests[0];
    expect(request?.url).toBe(`${BASE_URL}/api/v1/session`);
    expect(request?.method).toBe('POST');
    expect(request?.headers.get('Content-Type')).toBe('application/json');
    expect(await request?.json()).toEqual({ password: 'correct horse' });
  });

  it('returns invalid_credentials, which is not a session loss', async () => {
    const { client } = clientWith(() => Promise.resolve(problem(401, 'invalid_credentials')));
    const result = await client.login('wrong');
    expect(result).toMatchObject({ ok: false, error: { kind: 'problem', problem: { code: 'invalid_credentials' } } });
    if (!result.ok) {
      expect(isUnauthenticated(result.error)).toBe(false);
    }
  });

  it('returns login_throttled with retry_after_seconds', async () => {
    const { client } = clientWith(() => Promise.resolve(problem(429, 'login_throttled', { retry_after_seconds: 42 })));
    expect(await client.login('x')).toMatchObject({
      ok: false,
      error: { kind: 'problem', problem: { code: 'login_throttled', status: 429, retry_after_seconds: 42 } },
    });
  });

  it('rejects a 200 without a body', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(new Response('', { status: 200, headers: { 'Content-Length': '0' } })),
    );
    expect(await client.login('x')).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });

  it('returns network when no response arrives', async () => {
    const { client } = clientWith(() => Promise.reject(new TypeError('fetch failed')));
    expect(await client.login('x')).toEqual({ ok: false, error: { kind: 'network' } });
  });
});

describe('logout', () => {
  it('succeeds with 204 and an empty body', async () => {
    const { client, requests } = clientWith(() => Promise.resolve(new Response(null, { status: 204 })));
    expect(await client.logout()).toEqual({ ok: true, data: undefined });
    expect(requests[0]?.url).toBe(`${BASE_URL}/api/v1/session`);
    expect(requests[0]?.method).toBe('DELETE');
  });

  it.each([
    ['a 200 with an empty body', () => new Response('', { status: 200 })],
    ['a 200 with Content-Length 0', () => new Response('', { status: 200, headers: { 'Content-Length': '0' } })],
    ['a 200 with an empty object', () => json(200, {})],
    ['a 205', () => new Response(null, { status: 205 })],
  ])('rejects %s', async (_name, response) => {
    const { client } = clientWith(() => Promise.resolve(response()));
    expect(await client.logout()).toMatchObject({ ok: false, error: { kind: 'invalid_response' } });
  });

  it('returns the problem forbidden_origin for a 403', async () => {
    const { client } = clientWith(() => Promise.resolve(problem(403, 'forbidden_origin')));
    expect(await client.logout()).toMatchObject({
      ok: false,
      error: { kind: 'problem', problem: { code: 'forbidden_origin' } },
    });
  });
});

describe('isUnauthenticated', () => {
  it.each([
    ['unauthenticated', true],
    ['invalid_credentials', false],
    ['login_throttled', false],
    ['forbidden_origin', false],
  ] as const)('classifies the problem code %s', (code, expected) => {
    const error: ApiError = {
      kind: 'problem',
      problem: { type: 'about:blank', title: 'x', status: 401, code, detail: 'x' },
    };
    expect(isUnauthenticated(error)).toBe(expected);
  });

  it('is false for failures without a problem', () => {
    expect(isUnauthenticated({ kind: 'network' })).toBe(false);
    expect(isUnauthenticated({ kind: 'timeout' })).toBe(false);
    expect(isUnauthenticated({ kind: 'invalid_response', status: 401 })).toBe(false);
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

describe('getBattery', () => {
  const noDataYet = {
    state: 'no_data_yet',
    generated_at: '2026-11-27T12:00:00Z',
    range: { key: '7d', from: '2026-11-20T12:00:00Z', to: '2026-11-27T12:00:00Z', bucket_seconds: 600 },
  };

  it('sends the range as a query parameter and returns a valid body', async () => {
    const { client, requests } = clientWith(() => Promise.resolve(json(200, noDataYet)));
    expect(await client.getBattery('7d')).toEqual({ ok: true, data: noDataYet });
    expect(requests[0]?.url).toBe(`${BASE_URL}/api/v1/battery?range=7d`);
    expect(requests[0]?.credentials).toBe('same-origin');
  });

  it('rejects an unknown state as an invalid response, never as data', async () => {
    const { client } = clientWith(() => Promise.resolve(json(200, { ...noDataYet, state: 'empty' })));
    expect(await client.getBattery('7d')).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });

  it('rejects an available body without its series', async () => {
    const { client } = clientWith(() => Promise.resolve(json(200, { ...noDataYet, state: 'available' })));
    expect(await client.getBattery('7d')).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });

  it('rejects an empty 2xx body as an invalid response', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(new Response(null, { status: 200, headers: { 'Content-Length': '0' } })),
    );
    expect(await client.getBattery('7d')).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });

  it('returns a 503 problem as a problem', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(problem(503, 'database_unavailable', { retry_after_seconds: 30 })),
    );
    const result = await client.getBattery('24h');
    expect(result.ok).toBe(false);
    expect(result.ok ? null : result.error.kind).toBe('problem');
  });
});

describe('getConsumption', () => {
  const body = {
    generated_at: '2026-11-27T12:00:00Z',
    range: { key: '7d', from: '2026-11-20T12:00:00Z', to: '2026-11-27T12:00:00Z' },
    units: { distance: 'mi', energy: 'kWh', consumption: 'Wh/km', temperature: '°C', speed: 'km/h' },
    drives: [],
    totals: { drives: 0, distance: null, energy_used: null, consumption: null, drives_without_consumption: 0 },
    truncated: false,
  };

  it('sends the range as a query parameter and returns a valid body', async () => {
    const { client, requests } = clientWith(() => Promise.resolve(json(200, body)));
    expect(await client.getConsumption('7d')).toEqual({ ok: true, data: body });
    expect(requests[0]?.url).toBe(`${BASE_URL}/api/v1/consumption?range=7d`);
  });

  it('rejects a body with another distance unit, never shows it as data', async () => {
    const { client } = clientWith(() =>
      Promise.resolve(json(200, { ...body, units: { ...body.units, distance: 'km' } })),
    );
    expect(await client.getConsumption('7d')).toEqual({ ok: false, error: { kind: 'invalid_response', status: 200 } });
  });
});
