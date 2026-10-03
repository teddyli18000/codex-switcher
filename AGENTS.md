# Teddy fork guidance

This fork is a personal build. Read [docs/FORK_POLICY.md](docs/FORK_POLICY.md)
before changing networking, usage display, authentication, warm-up, or releases.

- Usage, subscription metadata, statistics, and reset-credit queries require an
  explicit refresh action. Startup, opening a view, switching accounts, imports,
  login completion, and cache expiry must not initiate these queries.
- Only manual warm-up and explicitly enabled clock-time warm-up are supported.
  The schedule belongs to the running application. Never install system tasks,
  background services, startup entries, or missed-run catch-up jobs for it.
- No updater, update checks, telemetry, analytics reporting, or remote UI assets.
  Necessary OAuth requests belong to user-initiated actions or enabled warm-up.
- Keep a shared persistent usage cache, with timestamps and bounded validity.
  Expired values are hidden; expiry never causes a network refresh.
- Preserve account storage and OAuth rotation/switch serialization. Do not read
  real user credentials or query real accounts for automated tests.
- Validate with existing checks and focused cache/schedule/network regression
  tests. Release builds must use the committed source, without CI source patches.

Keep these rules when integrating upstream updates. Update the policy only when
the user changes the product requirements.
