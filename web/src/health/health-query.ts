// The query of GET /health. The shell reads it for the reload banner, and
// the start page shows it as the state of the service.
import { queryOptions } from '@tanstack/react-query';
import { api, unwrap } from 'telemetry-web-api-client';

export const healthQuery = queryOptions({
  queryKey: ['health'] as const,
  queryFn: async ({ signal }) => unwrap(await api.getHealth({ signal })),
});
