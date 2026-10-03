import type { CachedAccountData, UsageInfo } from "../types";
import { getAvailableResetCredits } from "./resetCredits.ts";
import { invokeBackend } from "./platform.ts";

export const ACCOUNT_CACHE_TTL_MS = 5 * 60 * 1000;
export const ACCOUNT_CACHE_CHANGED_EVENT = "account-cache-changed";

type BackendInvoker = <T>(
  command: string,
  args?: Record<string, unknown>,
) => Promise<T>;

export function isCacheTimestampFresh(
  fetchedAt: number | null,
  now = Date.now(),
): boolean {
  if (fetchedAt === null || !Number.isFinite(fetchedAt)) return false;
  const age = now - fetchedAt;
  return age >= 0 && age < ACCOUNT_CACHE_TTL_MS;
}

function hideResetWindows(usage: UsageInfo, now: number): UsageInfo {
  const nowSeconds = now / 1000;
  const primaryExpired =
    usage.primary_resets_at !== null &&
    usage.primary_resets_at !== undefined &&
    usage.primary_resets_at <= nowSeconds;
  const secondaryExpired =
    usage.secondary_resets_at !== null &&
    usage.secondary_resets_at !== undefined &&
    usage.secondary_resets_at <= nowSeconds;

  if (!primaryExpired && !secondaryExpired) return usage;

  return {
    ...usage,
    ...(primaryExpired
      ? {
          primary_used_percent: null,
          primary_window_minutes: null,
          primary_resets_at: null,
        }
      : {}),
    ...(secondaryExpired
      ? {
          secondary_used_percent: null,
          secondary_window_minutes: null,
          secondary_resets_at: null,
        }
      : {}),
  };
}

export function getCachedAccountDataForDisplay(
  entry: CachedAccountData,
  now = Date.now(),
): CachedAccountData {
  const usageIsFresh =
    entry.usage?.account_id === entry.account_id &&
    isCacheTimestampFresh(entry.usage_fetched_at, now);
  const statsIsFresh =
    entry.stats?.account_id === entry.account_id &&
    isCacheTimestampFresh(entry.stats_fetched_at, now);
  const subscriptionExpiry = entry.metadata?.subscription_expires_at
    ? Date.parse(entry.metadata.subscription_expires_at)
    : NaN;
  const crossedSubscriptionExpiry =
    entry.metadata_fetched_at !== null &&
    subscriptionExpiry > entry.metadata_fetched_at && subscriptionExpiry <= now;
  const metadataIsFresh =
    entry.metadata !== null &&
    isCacheTimestampFresh(entry.metadata_fetched_at, now) &&
    !crossedSubscriptionExpiry;

  const stats = statsIsFresh && entry.stats ? { ...entry.stats } : null;
  if (stats?.reset_credits) {
    const credits = getAvailableResetCredits(stats.reset_credits, now);
    stats.reset_credits = {
      ...stats.reset_credits,
      available_count: credits.length,
      next_expires_at: credits.find((credit) => credit.expires_at)?.expires_at ?? null,
      credits,
    };
  }

  return {
    ...entry,
    usage: usageIsFresh && entry.usage ? hideResetWindows(entry.usage, now) : null,
    stats,
    metadata: metadataIsFresh ? entry.metadata : null,
  };
}

export function getCachedAccountDataMap(
  entries: CachedAccountData[],
  now = Date.now(),
): Map<string, CachedAccountData> {
  const byId = new Map<string, CachedAccountData>();
  for (const entry of entries) {
    byId.set(entry.account_id, getCachedAccountDataForDisplay(entry, now));
  }
  return byId;
}

export function formatCacheAge(
  fetchedAt: number | null,
  now = Date.now(),
): string {
  if (fetchedAt === null || !Number.isFinite(fetchedAt)) return "Never";
  const age = now - fetchedAt;
  if (age < 0) return "Time unavailable";
  const seconds = Math.floor(age / 1000);
  if (seconds < 5) return "Just now";
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return new Date(fetchedAt).toLocaleDateString();
}

export function getNextAccountCacheExpiry(
  entries: CachedAccountData[],
  now = Date.now(),
): number | null {
  let next: number | null = null;
  const consider = (timestamp: number | null | undefined) => {
    if (timestamp === null || timestamp === undefined || !Number.isFinite(timestamp)) return;
    if (timestamp > now && (next === null || timestamp < next)) next = timestamp;
  };

  for (const entry of entries) {
    for (const fetchedAt of [
      entry.usage_fetched_at,
      entry.stats_fetched_at,
      entry.metadata_fetched_at,
    ]) {
      if (isCacheTimestampFresh(fetchedAt, now)) consider(fetchedAt! + ACCOUNT_CACHE_TTL_MS);
    }
    consider(
      entry.usage?.primary_resets_at == null ? null : entry.usage.primary_resets_at * 1000,
    );
    consider(
      entry.usage?.secondary_resets_at == null ? null : entry.usage.secondary_resets_at * 1000,
    );
    if (entry.metadata?.subscription_expires_at) {
      const expiresAt = Date.parse(entry.metadata.subscription_expires_at);
      if (entry.metadata_fetched_at !== null && expiresAt > entry.metadata_fetched_at) consider(expiresAt);
    }
    for (const credit of entry.stats?.reset_credits?.credits ?? []) {
      if (!credit.expires_at) continue;
      const timestamp = new Date(credit.expires_at).getTime();
      if (Number.isFinite(timestamp)) consider(timestamp);
    }
  }
  return next;
}

export async function readCachedAccountData(
  invoke: BackendInvoker = invokeBackend,
): Promise<CachedAccountData[]> {
  return invoke<CachedAccountData[]>("get_cached_account_data");
}
