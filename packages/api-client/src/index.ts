// The handwritten facade of the generated API client.
//
// The web application imports only this module. The facade sets the
// runtime configuration, runs the generated Zod validators on every
// response body, and turns every failure into one `ApiError` value.
//
// Classification rule of a failed request:
// - The timeout signal fired: `timeout`.
// - The caller signal fired: the facade throws the abort reason. A
//   cancelled request has no result; TanStack Query discards it.
// - No HTTP response arrived (DNS, TLS, connection, CORS): `network`.
// - A 2xx response with a body that is not valid JSON or does not match
//   the success schema: `invalid_response`.
// - A non-2xx response with a valid Problem body whose `status` equals the
//   HTTP status: `problem`.
// - Any other non-2xx response: `invalid_response`. This includes a
//   problem with an unknown `code`, and a 502 HTML or text page of an
//   intermediary (the Railway or Cloudflare proxy). The status of such a
//   response is kept, but the body is not trusted.
// The first import: the Zod configuration must run before the generated
// schemas are built.
import './zod-config.js';
import { createClient, createConfig } from './generated/client/index.js';
import type { Client } from './generated/client/index.js';
import { getHealth as generatedGetHealth } from './generated/sdk.gen.js';
import type { Health, ProblemCode } from './generated/types.gen.js';
import { zProblem } from './generated/zod.gen.js';

export type { Health, ProblemCode };

/** A validated problem body: the output type of the generated schema. */
export type Problem = ReturnType<typeof zProblem.parse>;

/** The client deadline of one request, longer than the 30 s server timeout. */
export const DEFAULT_TIMEOUT_MS = 35_000;

export type ApiError =
  | { readonly kind: 'problem'; readonly problem: Problem }
  | { readonly kind: 'invalid_response'; readonly status: number | null }
  | { readonly kind: 'network' }
  | { readonly kind: 'timeout' };

export type ApiResult<T> = { readonly ok: true; readonly data: T } | { readonly ok: false; readonly error: ApiError };

/** The error that a query function throws for a failed request. */
export class ApiRequestError extends Error {
  readonly error: ApiError;

  constructor(error: ApiError) {
    super(`API request failed: ${error.kind}`);
    this.name = 'ApiRequestError';
    this.error = error;
  }
}

/** Returns the data of a result, or throws an `ApiRequestError`. */
export function unwrap<T>(result: ApiResult<T>): T {
  if (result.ok) {
    return result.data;
  }
  throw new ApiRequestError(result.error);
}

export type ApiClientOptions = {
  /** The origin of the API. The default empty string means the page origin. */
  readonly baseUrl?: string;
  /** The fetch function. The default is the global fetch. Tests inject a stub. */
  readonly fetch?: typeof globalThis.fetch;
  /** The deadline of one request in milliseconds. */
  readonly timeoutMs?: number;
};

export type RequestOptions = {
  /** The signal of the caller, for example the signal of TanStack Query. */
  readonly signal?: AbortSignal;
};

export type ApiClient = {
  readonly getHealth: (options?: RequestOptions) => Promise<ApiResult<Health>>;
};

// The result shape of a generated SDK function with `throwOnError: false`.
type GeneratedOutcome<T> = {
  readonly data?: T | undefined;
  readonly error?: unknown;
  readonly response?: Response | undefined;
};

export function createApiClient(options: ApiClientOptions = {}): ApiClient {
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const client: Client = createClient(
    createConfig({
      baseUrl: options.baseUrl ?? '',
      credentials: 'same-origin',
      // Always parse the body as JSON. With 'auto', a response with another
      // content type skips the response validator.
      parseAs: 'json',
      throwOnError: false,
      ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    }),
  );

  // One signal aborts the request at the deadline or when the caller
  // cancels. The facade also races the call against that signal, so the
  // result does not depend on how fetch and the body read react to the
  // abort. Node, for example, can lose the link between the signal and a
  // Request after a garbage collection.
  async function run<T>(
    caller: AbortSignal | undefined,
    call: (signal: AbortSignal) => Promise<GeneratedOutcome<T>>,
  ): Promise<ApiResult<T>> {
    if (caller?.aborted === true) {
      throw caller.reason;
    }
    const controller = new AbortController();
    let cause: 'timeout' | 'caller' | undefined;
    let wake: () => void = () => undefined;
    const aborted = new Promise<undefined>((resolve) => {
      wake = () => {
        resolve(undefined);
      };
    });
    const stop = (reason: 'timeout' | 'caller'): void => {
      cause ??= reason;
      controller.abort(
        reason === 'caller' ? caller?.reason : new DOMException('The request timed out.', 'TimeoutError'),
      );
      wake();
    };
    const onCallerAbort = (): void => {
      stop('caller');
    };
    const timer = setTimeout(() => {
      stop('timeout');
    }, timeoutMs);
    caller?.addEventListener('abort', onCallerAbort, { once: true });
    let outcome: GeneratedOutcome<T> | undefined;
    try {
      outcome = await Promise.race([call(controller.signal), aborted]);
    } finally {
      clearTimeout(timer);
      caller?.removeEventListener('abort', onCallerAbort);
    }
    if (cause === 'caller') {
      throw caller?.reason;
    }
    if (cause === 'timeout' || outcome === undefined) {
      return { ok: false, error: { kind: 'timeout' } };
    }
    const response = outcome.response;
    if (response === undefined) {
      return { ok: false, error: { kind: 'network' } };
    }
    if (response.ok) {
      if (outcome.error === undefined && outcome.data !== undefined) {
        return { ok: true, data: outcome.data };
      }
      return { ok: false, error: { kind: 'invalid_response', status: response.status } };
    }
    return { ok: false, error: classifyErrorBody(response.status, outcome.error) };
  }

  return {
    getHealth: async (request = {}) =>
      run<Health>(request.signal, async (signal) => generatedGetHealth({ client, signal })),
  };
}

function classifyErrorBody(status: number, body: unknown): ApiError {
  const parsed = zProblem.safeParse(body);
  if (parsed.success && parsed.data.status === status) {
    return { kind: 'problem', problem: parsed.data };
  }
  return { kind: 'invalid_response', status };
}

/**
 * Whether one retry of a read can help (design §7): the server was busy or
 * unavailable, or the network failed. A write needs its own idempotency rule.
 */
export function isRetryable(error: ApiError): boolean {
  switch (error.kind) {
    case 'network':
      return true;
    case 'timeout':
    case 'invalid_response':
      return false;
    case 'problem':
      return isRetryableProblem(error.problem.code);
    default:
      return unknownValue(error);
  }
}

function isRetryableProblem(code: ProblemCode): boolean {
  switch (code) {
    case 'database_unavailable':
    case 'service_busy':
    case 'timeout':
      return true;
    case 'unauthenticated':
    case 'invalid_credentials':
    case 'login_throttled':
    case 'forbidden_origin':
    case 'invalid_request':
    case 'unsupported_media_type':
    case 'payload_too_large':
    case 'not_found':
    case 'method_not_allowed':
    case 'data_contract_violation':
    case 'internal_error':
      return false;
    default:
      return unknownValue(code);
  }
}

// The fallback of an exhaustive switch. The type is `never` at compile
// time; at run time a value outside the closed set gives the safe answer.
function unknownValue(_value: never): false {
  return false;
}

/** The client of the page origin. */
export const api: ApiClient = createApiClient();
