# Personal fork: network and cache contract

Confirmed with Teddy on 2026-10-03. Base: upstream-compatible main at
`59bce4f` (v0.2.20). This document describes required behavior, not an assertion
that every upstream version already complies.

## Network actions

| Action | Permitted external requests |
| --- | --- |
| Startup, idle, show window/tray, expand statistics | None |
| Read cache, expiry, local settings or account list | None |
| Import a local auth file, rename/delete/export | No usage or metadata query |
| User refreshes quota, metadata, or statistics | Requested queries; necessary OAuth token refresh |
| User logs in, imports a refresh-token account, or switches account | Necessary OAuth requests; no follow-up usage query |
| Manual warm-up | Minimal model request; necessary OAuth token refresh |
| User enables warm-up at specified clock times | The same warm-up requests, only while the software runs |
| Update checks, telemetry, remote UI resources | Never |

Native tray, tray popup, main UI, and browser mode must follow the same rules.
Do not hide a network query inside a command named as a local read. Bounded
authentication retries belong to the initiating action; do not create idle
token-maintenance loops. Keep existing active-session token safety checks.

## Warm-up

Keep manual single/all-account warm-up and daily clock-time warm-up. Remove
quota-window-driven auto warm-up, including its old settings and controls.
Clock-time scheduling defaults to off and remembers user-selected times. It
runs inside the application, including while the main window is hidden, and
stops on app exit. No OS scheduled task, service, startup registration, or
catch-up on the next launch. Each configured local-clock minute runs at most
once per day across restarts. Missed times during sleep or shutdown are skipped.

Warm-up must work without a cached usage result and must not query usage to
decide whether to send. A valid cached result indicating exhausted weekly quota
may skip that account. A successful warm-up invalidates affected usage/statistics
so the app does not present the pre-request values as current.

## Cache

Persist usage, statistics/reset credits, and subscription metadata locally, in a
versioned cache separate from credential storage. Main UI and tray share this
cache. Cache data must not contain access tokens, refresh tokens, API keys, or
raw authentication responses.

- Record a successful fetch time separately for each dataset. Initial TTL is
  five minutes. Restarting the app does not reset or discard a valid TTL.
- Hide expired numbers and show a simple refresh prompt, with the last successful
  refresh time/age when available. Label cached values as last-fetched snapshots.
- Invalidate quota data at the recorded reset boundary; never infer zero usage
  or a new quota window without a query. Do not display expired reset credits.
- A manual refresh requests fresh data even when the cache is valid. Failed
  requests do not extend cache validity or replace the last-success timestamp.
- Separate account identities, remove deleted-account cache, and reject stale
  async completions after deletion or identity changes. Token rotation alone
  must not invalidate an otherwise matching account identity.
- Future timestamps, clock rollback, unsupported schema, and damaged cache data
  must not make data valid. Reading or expiring cache never contacts a server.

## Upstream upgrades and releases

Review all network call sites after an upstream merge, including native tray,
popup, metadata, statistics, OAuth, and warm-up. Remove updater component,
plugin dependencies, registration, permissions, endpoint config, and updater
artifacts; a hidden update button is insufficient. No telemetry may be added.

Old v1/Usage installers were based on v0.2.2 source `2ae5b7d` and a build-time
patch; the release tag points elsewhere. Old develop documentation claims manual
usage but its source still contains a startup/minute refresh. Do not use either
as the authoritative implementation. Build the checked-in source and record the
exact source commit with any installer. Update README when behavior changes.

## Required validation

- Launch/reopen/idle/import/show statistics and tray do not issue usage requests.
- Explicit refresh still requests the selected data and updates timestamp/cache.
- Restart recovers valid cache; expiry/reset/clock rollback hide invalid values.
- Deleted or replaced accounts cannot receive late results or another cache.
- Disabled schedule does nothing; enabled schedule works with no usage cache,
  skips missed minutes, and deduplicates restarts. No scheduler touches the OS.
- Account switching and OAuth rotation remain covered by existing backend tests.
- Run frontend tests/build and Rust tests/checks. Report browser/mock evidence
  separately from native UI or live-account evidence.
