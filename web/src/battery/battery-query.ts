// The query of `GET /api/v1/battery` for one range. The defaults of the
// query client apply (query-client.ts): no refetch on focus, reconnect, or
// an interval; one retry only for a busy or unavailable server or a
// network failure. A navigation to another range and the "Päivitä"
// button are the only triggers.
import { queryOptions } from '@tanstack/react-query';
import { api, unwrap } from 'telemetry-web-api-client';
import type { RangeKey } from 'telemetry-web-api-client';

export function batteryQuery(range: RangeKey) {
  return queryOptions({
    queryKey: ['battery', range] as const,
    queryFn: async ({ signal }) => unwrap(await api.getBattery(range, { signal })),
  });
}
