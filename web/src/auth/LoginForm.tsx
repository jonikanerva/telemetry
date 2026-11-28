// The login form: POST /api/v1/session through a mutation.
//
// - Success: the session body goes into the query cache, the mark of an
//   earlier failed logout is cleared (auth/logout.ts), and the browser
//   opens the target page with `replace`, so the back button does not
//   return to the login page.
// - Failure: the message of auth/login-failure.ts, focus to the password
//   field. Only a wrong password clears the field.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useRef, useState } from 'react';
import { api, unwrap } from 'telemetry-web-api-client';
import type { Session } from 'telemetry-web-api-client';

import { classifyLoginFailure, clearsPassword, loginFailureMessage } from './login-failure.js';
import { LoginFormView } from './LoginFormView.js';
import { clearLogoutFailure } from './logout.js';
import { SESSION_QUERY_KEY } from './session.js';

export function LoginForm({ redirect }: { readonly redirect: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);
  const [password, setPassword] = useState('');
  const [message, setMessage] = useState<string | null>(null);

  const login = useMutation({
    mutationFn: async (value: string): Promise<Session> => unwrap(await api.login(value)),
    onSuccess: async (session) => {
      queryClient.setQueryData<Session>(SESSION_QUERY_KEY, session);
      clearLogoutFailure();
      await navigate({ href: redirect, replace: true });
    },
    onError: (error) => {
      const failure = classifyLoginFailure(error);
      setMessage(loginFailureMessage(failure));
      if (clearsPassword(failure)) {
        setPassword('');
      }
      inputRef.current?.focus();
    },
  });

  return (
    <LoginFormView
      password={password}
      pending={login.isPending}
      message={message}
      inputRef={inputRef}
      onPasswordChange={setPassword}
      onSubmit={(event) => {
        event.preventDefault();
        if (login.isPending) {
          return;
        }
        // Remove the old message, so that the same message is announced
        // again after the next failure.
        setMessage(null);
        login.mutate(password);
      }}
    />
  );
}
