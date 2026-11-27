# ADR 0003: Web access control

- Status: Accepted, 27 November 2026. Owner decision. It closes the open
  question "Access control for the web application" of `VISION.md`.
- Scope: who can open the web application and how the owner signs in. The
  first web PR owns the implementation. This ADR names no crate and no
  code.

## Context

- The data shows where the owner is and when the owner is away
  (`VISION.md`, "Private by default"). A public URL without access control
  is a sign of drift (`VISION.md`, "Non-Goals & Drift Guardrails").
- One user: the owner. The product has no accounts, no sharing, and no
  second user (`VISION.md`).
- The product stays in the free tiers and adds no new external provider
  without owner approval (`VISION.md`).
- The earlier proposal was Cloudflare Access. The Cloudflare Zero Trust
  set-up requires a payment method also for the free plan. Step "Create a
  Zero Trust organization": "Complete your onboarding by selecting a
  subscription plan and entering your payment details." The next sentence:
  "If you chose the Zero Trust Free plan, this step is still needed but
  you will not be charged." [CF]
- The web application runs on Railway behind the Railway proxy. An IP
  address of a request is therefore not a reliable identity for a limit.
- `STACK.md` §2.5 names OIDC with an approved provider as the profile
  default for accounts. A first-party session system needs a demonstrated
  need, a threat model, an independent security review, and end-to-end
  evidence.

## Decision

- One password protects the web application.
- The application stores no password. A Railway service variable of the
  web service holds an Argon2id hash of the password. The owner creates
  the hash on the owner's computer.
- The owner uses a long random password from a password manager. There is
  no second factor.
- After a correct login, the server gives the browser a session cookie
  with the attributes `Secure`, `HttpOnly`, and `SameSite`. The session
  has an expiry. The owner can log out.
- The server slows down repeated login attempts. The limit is global, not
  for each IP address.
- Where the server keeps session data is a technical choice of the first
  web PR.
- The demonstrated need for a first-party session system under
  `STACK.md` §2.5: the owner does not use Cloudflare Access now, because
  it needs a payment method. The product has one user and no accounts, so
  an OIDC provider adds an external provider for one person. The other requirements of
  §2.5 (threat model, independent security review, end-to-end evidence,
  and the cookie, CSRF, rotation, fixation, expiry, revocation, and logout
  rules) apply to the first web PR.

## Key alternatives

- **Cloudflare Access.** No first-party login code, and a second factor is
  possible. It needs a payment method on the Cloudflare account, also for
  the free plan [CF]. Not used now. It can go in front of the application
  later without an application change.
- **OIDC with a provider.** The `STACK.md` §2.5 default. It adds an
  external provider and a login flow for one user. Rejected.
- **HTTP basic authentication.** Simple, but the browser sends the
  password with each request, has no real logout, and keeps no session
  with an expiry. Rejected.

## Consequences

- The login page is reachable from the internet.
- The login, the session, and the throttle are first-party security code.
  They need the evidence of `STACK.md` §2.5 in the first web PR.
- There is no second factor. The protection depends on the length and the
  secrecy of the password.
- A global throttle also slows down the owner's own login while somebody
  else sends many attempts.
- Railway gives a service variable to the processes of the web service.
  A holder of the hash can try passwords offline. A long random password
  keeps this impractical.
- A password change needs a new variable value and a redeploy of the web
  service.

## Cost of reversal

Low while the web application is small. Cloudflare Access can go in front
without an application change. A change to OIDC replaces the login and the
session code of the web service. No stored data depends on this decision.

## Reassessment

- The owner wants a second factor.
- A security finding in the login path.
- A second user is needed. `VISION.md` forbids a second user today.

## Sources

- [CF] Cloudflare Zero Trust, "Get started", section "Create a Zero Trust
  organization", read on 27 November 2026:
  <https://developers.cloudflare.com/cloudflare-one/setup/>
