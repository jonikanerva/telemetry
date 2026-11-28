// The markup of the logout failure page, without state.
// LogoutFailure.tsx owns the state; the unit tests render this view to
// static markup.
//
// Accessibility: one `h1` outside the alert region. One `role="alert"`
// region always exists, so a new message is announced. While the request
// runs, the button has `aria-disabled` and a busy text. A `disabled`
// button would lose the focus of a keyboard user; with `aria-disabled`
// the button keeps focus, also after a failed retry, and LogoutFailure
// ignores a second click.
import { fi } from '../i18n/fi.js';

export type LogoutFailureViewProps = {
  readonly pending: boolean;
  /** The text of the alert region, or null while a retry runs. */
  readonly message: string | null;
  readonly onRetry: () => void;
};

export function LogoutFailureView(props: LogoutFailureViewProps) {
  return (
    <section className="logout-failure" aria-labelledby="logout-failure-title">
      <h1 id="logout-failure-title">{fi.logoutFailedTitle}</h1>
      <div role="alert" className="logout-failure-alert">
        {props.message === null ? null : <p className="notice notice-error">{props.message}</p>}
      </div>
      <button type="button" aria-disabled={props.pending ? true : undefined} onClick={props.onRetry}>
        {props.pending ? fi.logoutPending : fi.logoutRetry}
      </button>
    </section>
  );
}
