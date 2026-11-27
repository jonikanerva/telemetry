// Values that the auth tests share. Test code only.
import { ApiRequestError } from 'telemetry-web-api-client';
import type { ApiError, ProblemCode, Session } from 'telemetry-web-api-client';

export const ORIGIN = 'https://telemetry.example';

export const SESSION: Session = {
  expires_at: '2026-12-27T08:00:00Z',
  idle_expires_at: '2026-12-04T08:00:00Z',
};

export function problemError(code: ProblemCode, status: number, retryAfterSeconds?: number): ApiRequestError {
  const error: ApiError = {
    kind: 'problem',
    problem: {
      type: 'about:blank',
      title: 'Reason',
      status,
      code,
      detail: 'Static text.',
      ...(retryAfterSeconds === undefined ? {} : { retry_after_seconds: retryAfterSeconds }),
    },
  };
  return new ApiRequestError(error);
}
