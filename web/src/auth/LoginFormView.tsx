// The markup of the login form, without state. LoginForm.tsx owns the
// state; the unit tests render this view to static markup.
//
// Accessibility: a visible label names the password field. One
// `role="alert"` region always exists, so a new message is announced.
// After a failure the field has `aria-invalid` and `aria-describedby`
// that points to the message. While the request runs, the form has
// `aria-busy` and the button has `aria-disabled` and a busy text. A
// `disabled` button would lose the focus of a keyboard user; with
// `aria-disabled` the button keeps focus and stays in the tab order, and
// LoginForm ignores a second submit.
import type { Ref, SubmitEvent } from 'react';

import { fi } from '../i18n/fi.js';

export const LOGIN_PASSWORD_ID = 'login-password';
export const LOGIN_MESSAGE_ID = 'login-message';
/** The longest password of the contract (LoginRequest.password). */
export const PASSWORD_MAX_LENGTH = 1024;

export type LoginFormViewProps = {
  readonly password: string;
  readonly pending: boolean;
  /** The failure message of the last attempt, or null. */
  readonly message: string | null;
  readonly inputRef?: Ref<HTMLInputElement>;
  readonly onPasswordChange: (password: string) => void;
  readonly onSubmit: (event: SubmitEvent<HTMLFormElement>) => void;
};

export function LoginFormView(props: LoginFormViewProps) {
  const failed = props.message !== null;
  return (
    <section className="login" aria-labelledby="login-title">
      <h1 id="login-title">{fi.loginTitle}</h1>
      {/* Outside the form: a screen reader can hold back a live region
          inside an `aria-busy` subtree. */}
      <div role="alert" className="login-alert">
        {failed ? (
          <p id={LOGIN_MESSAGE_ID} className="notice notice-error">
            {props.message}
          </p>
        ) : null}
      </div>
      <form className="login-form" aria-busy={props.pending} onSubmit={props.onSubmit}>
        <label htmlFor={LOGIN_PASSWORD_ID}>{fi.loginPasswordLabel}</label>
        <input
          ref={props.inputRef}
          id={LOGIN_PASSWORD_ID}
          name="password"
          type="password"
          autoComplete="current-password"
          required
          maxLength={PASSWORD_MAX_LENGTH}
          value={props.password}
          aria-invalid={failed ? true : undefined}
          aria-describedby={failed ? LOGIN_MESSAGE_ID : undefined}
          onChange={(event) => {
            props.onPasswordChange(event.currentTarget.value);
          }}
        />
        <button type="submit" aria-disabled={props.pending ? true : undefined}>
          {props.pending ? fi.loginSubmitting : fi.loginSubmit}
        </button>
      </form>
    </section>
  );
}
