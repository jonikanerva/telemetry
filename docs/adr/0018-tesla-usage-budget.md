# ADR 0018: Tesla usage budget and the lead's config authority

- Status: Accepted, 28 November 2026. Decided by: owner. The lead
  implemented the decision in the PR that adds the 201-field vehicle
  config.
- Scope: the monthly Tesla usage that the owner accepts, the payment
  method and the billing limit, and the bound of the lead's authority to
  change and send the vehicle telemetry config. Supersedes decision 1 of
  ADR 0004.

## Context

- ADR 0004, decision 1, let the lead send a config with an estimated usage
  below 3 USD each month. The config had 20 fields.
- The owner wants every field that the vehicle can send, for example the
  battery temperatures for the battery and consumption views. A field that
  is not in the config is never collected and cannot be collected later.
- Tesla prices streaming at 150,000 signals for 1 USD and gives a monthly
  discount of 10 USD. The default billing limit is 0. The limit can be
  raised after a payment method is added. When usage exceeds the limit,
  Tesla suspends the API and removes the telemetry config from the
  vehicle, and does not restore it
  (<https://developer.tesla.com/docs/fleet-api/billing-and-limits>, read
  28 November 2026). A payment method alone does not prevent the removal.
- The owner's Home Assistant uses the same application, discount, and
  limit. The owner read its usage on the Tesla dashboard as about 1 to 2
  EUR each month (28 November 2026).
- The model of `docs/telemetry-usage-estimate.md` gives these usages of
  the 201-field config: planning month 3.41 USD, road-trip month 6.02 USD,
  awake all month 5.39 USD, and the extreme month (road trip, awake all
  other hours) 7.85 USD.

## Decision

1. The owner accepts Tesla usage up to the monthly discount of 10 USD plus
   a paid overage of about 10 EUR each month.
2. The owner adds a payment method to the Tesla developer account and sets
   a billing limit with a wide margin above the highest modelled month,
   Home Assistant included. Reason: Tesla removes the config at the limit,
   also with a payment method on file.
3. The lead may change and send the vehicle telemetry config without a
   separate owner approval when the estimated usage of the new config
   stays below 8 USD each month in the highest modelled month of
   `docs/telemetry-usage-estimate.md`. The PR records the estimate. A
   config with a higher estimate needs the owner's approval.

## Key alternatives

- Keep the bound of 3 USD and send only the fields that fit (about 65
  fields). Rejected by the owner: the left-out fields cannot be collected
  later.
- Send every field at an interval of 1 s. Rejected: the model gives more
  than 100 USD each month.
- No payment method. Rejected: a month above the discount would remove
  the config.

## Consequences

- The product has a possible monthly cost. `VISION.md` needs the owner's
  approval of the changed budget principle.
- A billing limit that is too low removes the config and stops Home
  Assistant. The owner checks the dashboard after the first month.
- The database grows about 57 MB in the planning month, so the 1 GB of the
  Neon Free plan fills in about 17 months (ADR 0002, "Expected size").

## Cost of reversal

Low. The owner can lower the bound, remove the payment method, or have a
smaller config sent. Data that the vehicle sent stays stored.

## Reassessment

After the first full month with the 201-field config: compare the
dashboard usage with the model. Also when Tesla changes its prices or the
discount, or when the Home Assistant usage changes.
