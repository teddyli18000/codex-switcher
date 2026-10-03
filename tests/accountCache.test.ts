import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import {
  ACCOUNT_CACHE_TTL_MS,
  getCachedAccountDataForDisplay,
  getNextAccountCacheExpiry,
  isCacheTimestampFresh,
  readCachedAccountData,
} from "../src/lib/accountCache.ts";
import type { CachedAccountData } from "../src/types/index.ts";

function cacheEntry(now: number, overrides: Partial<CachedAccountData> = {}): CachedAccountData {
  return {
    account_id: "account-a",
    usage: {
      account_id: "account-a",
      plan_type: "plus",
      primary_used_percent: 40,
      primary_window_minutes: 300,
      primary_resets_at: Math.floor(now / 1000) + 10,
      secondary_used_percent: 70,
      secondary_window_minutes: 10080,
      secondary_resets_at: Math.floor(now / 1000) + 20,
      has_credits: null,
      unlimited_credits: null,
      credits_balance: null,
      error: null,
    },
    usage_fetched_at: now,
    stats: {
      account_id: "account-a",
      available: true,
      source: "test",
      generated_at: null,
      stats_as_of: null,
      summary: {
        lifetime_tokens: 10,
        peak_daily_tokens: 10,
        longest_task_seconds: null,
        current_streak_days: null,
        longest_streak_days: null,
      },
      activity: {
        fast_mode_percent: null,
        reasoning_effort: null,
        reasoning_effort_percent: null,
        skills_explored: null,
        total_skills_used: null,
        total_threads: null,
      },
      daily: [],
      top_invocations: [],
      reset_credits: {
        available_count: 2,
        next_expires_at: new Date(now + 20_000).toISOString(),
        credits: [
          {
            id: "valid-credit",
            reset_type: "codex_rate_limits",
            status: "available",
            granted_at: null,
            expires_at: new Date(now + 20_000).toISOString(),
            redeem_started_at: null,
            redeemed_at: null,
            title: null,
            description: null,
          },
          {
            id: "expired-credit",
            reset_type: "codex_rate_limits",
            status: "available",
            granted_at: null,
            expires_at: new Date(now).toISOString(),
            redeem_started_at: null,
            redeemed_at: null,
            title: null,
            description: null,
          },
        ],
      },
      error: null,
    },
    stats_fetched_at: now,
    metadata: { plan_type: "plus", subscription_expires_at: "2027-01-01T00:00:00Z" },
    metadata_fetched_at: now,
    ...overrides,
  };
}

test("cache freshness includes the last millisecond before TTL and expires at the TTL", () => {
  const now = 1_800_000_000_000;
  assert.equal(isCacheTimestampFresh(now - ACCOUNT_CACHE_TTL_MS + 1, now), true);
  assert.equal(isCacheTimestampFresh(now - ACCOUNT_CACHE_TTL_MS, now), false);
  assert.equal(isCacheTimestampFresh(now + 1, now), false);
});

test("expired values are hidden while successful-fetch timestamps remain visible", () => {
  const now = 1_800_000_000_000;
  const entry = cacheEntry(now, {
    usage_fetched_at: now - ACCOUNT_CACHE_TTL_MS,
    stats_fetched_at: now - ACCOUNT_CACHE_TTL_MS,
    metadata_fetched_at: now - ACCOUNT_CACHE_TTL_MS,
  });

  const displayed = getCachedAccountDataForDisplay(entry, now);
  assert.equal(displayed.usage, null);
  assert.equal(displayed.stats, null);
  assert.equal(displayed.metadata, null);
  assert.equal(displayed.usage_fetched_at, entry.usage_fetched_at);
  assert.equal(displayed.stats_fetched_at, entry.stats_fetched_at);
  assert.equal(displayed.metadata_fetched_at, entry.metadata_fetched_at);
});

test("reset boundaries hide only the reset usage window and expired reset credits", () => {
  const now = 1_800_000_000_000;
  const displayed = getCachedAccountDataForDisplay(cacheEntry(now), now);

  assert.equal(displayed.usage?.primary_used_percent, 40);
  assert.equal(displayed.usage?.secondary_used_percent, 70);
  assert.equal(displayed.stats?.reset_credits?.available_count, 1);
  assert.deepEqual(
    displayed.stats?.reset_credits?.credits.map((credit) => credit.id),
    ["valid-credit"],
  );

  const afterReset = getCachedAccountDataForDisplay(
    cacheEntry(now, {
      usage: {
        ...cacheEntry(now).usage!,
        primary_resets_at: now / 1000,
      },
    }),
    now,
  );
  assert.equal(afterReset.usage?.primary_used_percent, null);
  assert.equal(afterReset.usage?.secondary_used_percent, 70);
});

test("expiry scheduling picks the nearest valid TTL, quota, or credit deadline", () => {
  const now = 1_800_000_000_000;
  const entry = cacheEntry(now);
  assert.equal(
    getNextAccountCacheExpiry([entry], now),
    entry.usage!.primary_resets_at! * 1000,
  );
});

test("metadata expires when a future subscription boundary is crossed", () => {
  const now = 1_800_000_000_000;
  const entry = cacheEntry(now, {
    metadata: { plan_type: "plus", subscription_expires_at: new Date(now + 1000).toISOString() },
  });
  assert.equal(getNextAccountCacheExpiry([entry], now), now + 1000);
  assert.equal(getCachedAccountDataForDisplay(entry, now + 1000).metadata, null);
  entry.metadata_fetched_at = now + 1000;
  assert.notEqual(getCachedAccountDataForDisplay(entry, now + 1000).metadata, null);
});

test("cache reads call only the local cache command", async () => {
  const commands: string[] = [];
  await readCachedAccountData(async <T>(command: string) => {
    commands.push(command);
    return [] as T;
  });
  assert.deepEqual(commands, ["get_cached_account_data"]);
});

test("startup, statistics display, and tray loading have no background refresh call", async () => {
  const [accountsSource, statsSource, traySource] = await Promise.all([
    readFile(new URL("../src/hooks/useAccounts.ts", import.meta.url), "utf8"),
    readFile(new URL("../src/components/AccountUsageStats.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/TrayMenu.tsx", import.meta.url), "utf8"),
  ]);
  const trayLoadStart = traySource.indexOf("const load = useCallback");
  const trayRefreshStart = traySource.indexOf("// Manual refresh", trayLoadStart);
  const trayLoadSource = traySource.slice(trayLoadStart, trayRefreshStart);

  assert.doesNotMatch(accountsSource, /setInterval\s*\(/);
  assert.match(accountsSource, /void loadAccounts\(\);/);
  assert.doesNotMatch(statsSource, /usageChanged|backgroundInFlight|loadStats\(true\)/);
  assert.doesNotMatch(trayLoadSource, /get_usage|get_account_usage_stats|loadUsage|loadActiveStats/);
});
