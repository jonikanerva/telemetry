// The logout button of the header (flow: auth/logout.ts). The pending
// state is local React state, not a mutation: the flow clears the whole
// query client, including its mutation cache.
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useState } from 'react';
import { api } from 'telemetry-web-api-client';

import { fi } from '../i18n/fi.js';
import { performLogout } from './logout.js';

export function LogoutButton() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [pending, setPending] = useState(false);

  return (
    <button
      type="button"
      className="logout-button"
      // `aria-disabled`, not `disabled`: the clicked button keeps the
      // focus of a keyboard user. The handler ignores a second click.
      aria-disabled={pending ? true : undefined}
      onClick={() => {
        if (pending) {
          return;
        }
        setPending(true);
        void performLogout({
          logout: async () => api.logout(),
          queryClient,
          navigate: async (target) => navigate(target),
        }).finally(() => {
          setPending(false);
        });
      }}
    >
      {pending ? fi.logoutPending : fi.logout}
    </button>
  );
}
