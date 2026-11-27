// The logout failure page (flow: auth/logout.ts). The pending state and
// the message are local React state, not a mutation: a successful retry
// clears the whole query client, including its mutation cache. The page
// shows the failure message from the start, also when it is opened
// directly; it never sends a logout by itself.
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useRef, useState } from 'react';
import { api } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';
import { retryLogout } from './logout.js';
import type { LogoutAttempt } from './logout.js';
import { LogoutFailureView } from './LogoutFailureView.js';

export function LogoutFailure() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const attempt = useRef<LogoutAttempt>({ running: false });
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState<string | null>(fi.logoutFailedBody);

  return (
    <LogoutFailureView
      pending={pending}
      message={message}
      onRetry={() => {
        void retryLogout(attempt.current, {
          logout: async () => api.logout(),
          queryClient,
          navigate: async (target) => navigate(target),
          setPending,
          setMessage,
        });
      }}
    />
  );
}
