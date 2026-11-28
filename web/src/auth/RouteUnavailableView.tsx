// The markup of the unavailable state of a route, without state.
// RouteUnavailable.tsx chooses the body text; the unit tests render this
// view to static markup.
import { fi } from '../i18n/fi.js';

export type RouteUnavailableViewProps = {
  readonly body: string;
  readonly onRetry: () => void;
};

export function RouteUnavailableView({ body, onRetry }: RouteUnavailableViewProps) {
  return (
    <section aria-labelledby="unavailable-title">
      <div role="status" data-state="unavailable" className="notice notice-error">
        <h1 id="unavailable-title" className="notice-title">
          {fi.unavailableTitle}
        </h1>
        <p>{body}</p>
      </div>
      <button type="button" className="retry-button" onClick={onRetry}>
        {fi.retry}
      </button>
    </section>
  );
}
