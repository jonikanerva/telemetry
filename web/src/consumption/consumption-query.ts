// The query of `GET /api/v1/consumption` for one range. The defaults of
// the query client apply (query-client.ts), as in the battery view.
import { queryOptions } from '@tanstack/react-query';
import { api, unwrap } from 'telemetry-web-api-client';
import type { RangeKey } from 'telemetry-web-api-client';

export function consumptionQuery(range: RangeKey) {
  return queryOptions({
    queryKey: ['consumption', range] as const,
    queryFn: async ({ signal }) => unwrap(await api.getConsumption(range, { signal })),
  });
}
