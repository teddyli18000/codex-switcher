import { useCallback, useEffect, useState } from "react";
import type { AccountInfo, AccountUsageStats, DockDisplayMode, UsageInfo } from "./types";
import { invokeBackend, isTauriRuntime } from "./lib/platform";
import { formatCacheAge } from "./lib/accountCache";
import { useCachedAccountData } from "./hooks/useCachedAccountData";
import {
  applyTheme,
  syncThemeFromStorage,
  THEME_CHANGED_EVENT,
  type ThemeMode,
} from "./lib/theme";

const TRAY_REFRESH_EVENT = "tray-refresh";
const ACCOUNTS_CHANGED_EVENT = "accounts-changed";
const SWITCH_ACCOUNT_BLOCKED_EVENT = "switch-account-blocked";
// Mirrors the backend guard message in process.rs (ensure_codex_not_running).
const CODEX_RUNNING_PREFIX = "Cannot switch accounts while";

function formatError(err: unknown): string {
  if (!err) return "Unknown error";
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return "Unknown error";
  }
}

// "plus" -> "Plus". Returns null when there is no usable plan label.
function formatPlan(plan: string | null): string | null {
  const trimmed = plan?.trim();
  if (!trimmed) return null;
  return trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

// Color classes for a rate-limit window based on remaining %, matching the main app.
function remainingTone(remaining: number): { text: string; bar: string; dot: string } {
  if (remaining <= 10) {
    return { text: "text-red-500 dark:text-red-400", bar: "bg-red-500", dot: "bg-red-500" };
  }
  if (remaining <= 30) {
    return {
      text: "text-amber-500 dark:text-amber-400",
      bar: "bg-amber-500",
      dot: "bg-amber-500",
    };
  }
  return {
    text: "text-green-600 dark:text-green-400",
    bar: "bg-emerald-500",
    dot: "bg-emerald-500",
  };
}

// "time until reset" label, e.g. "4h 55m" / "4d 18h" / "now".
function formatResetAt(resetAt: number | null | undefined): string | null {
  if (!resetAt) return null;

  const diff = resetAt - Math.floor(Date.now() / 1000);
  if (diff <= 0) return "now";
  if (diff < 60) return `${diff}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86_400) {
    return `${Math.floor(diff / 3600)}h ${Math.floor((diff % 3600) / 60)}m`;
  }
  return `${Math.floor(diff / 86_400)}d ${Math.floor((diff % 86_400) / 3600)}h`;
}

function formatExactResetTime(
  resetAt: number | null | undefined,
  isWeekly: boolean,
): string | null {
  if (!resetAt) return null;

  const date = new Date(resetAt * 1000);
  const diff = resetAt - Math.floor(Date.now() / 1000);

  if (isWeekly && diff > 86_400) {
    return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(date);
  }

  const minutes = String(date.getMinutes()).padStart(2, "0");
  const period = date.getHours() >= 12 ? "PM" : "AM";
  const hour12 = date.getHours() % 12 || 12;

  return `${hour12}:${minutes} ${period}`;
}

function formatTokens(tokens: number | null | undefined): string {
  if (tokens === null || tokens === undefined || !Number.isFinite(tokens)) return "--";
  const abs = Math.abs(tokens);
  if (abs >= 1_000_000_000) return `${(tokens / 1_000_000_000).toFixed(1)}B`;
  if (abs >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (abs >= 1_000) return `${(tokens / 1_000).toFixed(1)}K`;
  return `${tokens}`;
}

function dayKey(offset: number): string {
  const date = new Date();
  date.setDate(date.getDate() - offset);
  const year = date.getFullYear();
  const month = `${date.getMonth() + 1}`.padStart(2, "0");
  const day = `${date.getDate()}`.padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function sumDailyTokens(stats: AccountUsageStats, days: number): number {
  const keys = new Set(Array.from({ length: days }, (_, index) => dayKey(index)));
  return stats.daily.reduce((total, day) => (keys.has(day.date) ? total + day.tokens : total), 0);
}

function TrayMenu() {
  const [accounts, setAccounts] = useState<AccountInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [switchingId, setSwitchingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshErrors, setRefreshErrors] = useState<Record<string, string | null>>({});
  const [refreshing, setRefreshing] = useState(false);
  const [dockDisplayMode, setDockDisplayMode] = useState<DockDisplayMode | null>(null);
  const { cacheByAccountId, error: cacheError, reload: reloadCache } = useCachedAccountData();

  const loadUsage = useCallback(async (list: AccountInfo[]) => {
    const errors = new Map<string, string>();
    await Promise.all(list.map(async (account) => {
      try {
        const usage = await invokeBackend<UsageInfo>("get_usage", {
          accountId: account.id,
        });
        if (usage.error) errors.set(account.id, usage.error);
      } catch (err) {
        errors.set(account.id, formatError(err));
      }
    }));
    return errors;
  }, []);

  const loadActiveStats = useCallback(async (list: AccountInfo[]) => {
    const active = list.find((account) => account.is_active);
    if (!active || active.auth_mode !== "chat_g_p_t") return new Map<string, string>();

    try {
      const stats = await invokeBackend<AccountUsageStats>("get_account_usage_stats", {
        accountId: active.id,
      });
      return stats.error ? new Map([[active.id, stats.error]]) : new Map<string, string>();
    } catch (err) {
      return new Map([[active.id, formatError(err)]]);
    }
  }, []);

  const loadDockDisplayMode = useCallback(async () => {
    try {
      const mode = await invokeBackend<DockDisplayMode | null>("get_dock_display_mode");
      setDockDisplayMode(mode);
    } catch {
      setDockDisplayMode(null);
    }
  }, []);

  const load = useCallback(async () => {
    try {
      void loadDockDisplayMode();
      const list = await invokeBackend<AccountInfo[]>("list_accounts");
      setAccounts(list);
      setRefreshErrors((prev) =>
        Object.fromEntries(Object.entries(prev).filter(([id]) => list.some((account) => account.id === id))),
      );
      setError(null);
      await reloadCache();
    } catch (err) {
      setError(formatError(err));
    } finally {
      setLoading(false);
    }
  }, [loadDockDisplayMode, reloadCache]);

  // Manual refresh is the only tray action that fetches usage or statistics.
  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const list = await invokeBackend<AccountInfo[]>("list_accounts");
      setAccounts(list);
      setError(null);
      const [usageErrors, statsErrors] = await Promise.all([
        loadUsage(list),
        loadActiveStats(list),
      ]);
      const errors = new Map([...usageErrors, ...statsErrors]);
      setRefreshErrors((prev) => {
        const next = { ...prev };
        for (const account of list) next[account.id] = errors.get(account.id) ?? null;
        return next;
      });
      await reloadCache();
      if (errors.size > 0) {
        setError(`Refresh failed for ${errors.size} account${errors.size === 1 ? "" : "s"}.`);
      }
    } catch (err) {
      setError(formatError(err));
    } finally {
      setRefreshing(false);
    }
  }, [loadActiveStats, loadUsage, reloadCache]);

  const handleDockDisplayMode = useCallback(
    async (mode: DockDisplayMode) => {
      const previous = dockDisplayMode;
      setDockDisplayMode(mode);
      try {
        const next = await invokeBackend<DockDisplayMode | null>("set_dock_display_mode", {
          mode,
        });
        setDockDisplayMode(next);
      } catch (err) {
        setDockDisplayMode(previous);
        setError(formatError(err));
      }
    },
    [dockDisplayMode]
  );

  useEffect(() => {
    void load();
  }, [load]);

  // Reload when the tray is reopened or accounts change elsewhere.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    let unlistenRefresh: (() => void) | undefined;
    let unlistenChanged: (() => void) | undefined;
    let unlistenTheme: (() => void) | undefined;

    void (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlistenRefresh = await listen(TRAY_REFRESH_EVENT, () => {
        syncThemeFromStorage();
        void load();
      });
      unlistenChanged = await listen(ACCOUNTS_CHANGED_EVENT, () => void load());
      unlistenTheme = await listen<ThemeMode>(THEME_CHANGED_EVENT, ({ payload }) => {
        if (payload === "light" || payload === "dark") {
          applyTheme(payload);
        }
      });
    })();

    return () => {
      unlistenRefresh?.();
      unlistenChanged?.();
      unlistenTheme?.();
    };
  }, [load]);

  const handleSwitch = useCallback(async (account: AccountInfo) => {
    if (account.is_active) {
      void invokeBackend("hide_tray_window");
      return;
    }
    try {
      setSwitchingId(account.id);
      setError(null);
      await invokeBackend("switch_account", { accountId: account.id });
      // Notify the main window immediately so its active-account state stays in
      // sync without waiting on the backend accounts-file watcher (~1s poll).
      const { emit } = await import("@tauri-apps/api/event");
      await emit(ACCOUNTS_CHANGED_EVENT);
      void invokeBackend("hide_tray_window");
    } catch (err) {
      const message = formatError(err);
      // Codex is running: hand off to the main window's force-close flow.
      if (message.startsWith(CODEX_RUNNING_PREFIX)) {
        const { emit } = await import("@tauri-apps/api/event");
        await emit(SWITCH_ACCOUNT_BLOCKED_EVENT, {
          accountId: account.id,
          error: message,
        });
        void invokeBackend("open_main_window"); // focus main + hide tray
        return;
      }
      setError(message);
    } finally {
      setSwitchingId(null);
    }
  }, []);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden rounded-xl border border-gray-200 bg-white text-gray-900 shadow-2xl dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100">
      <div className="flex items-center gap-2 border-b border-gray-100 px-3 py-2 dark:border-gray-800">
        <div className="flex h-6 w-6 items-center justify-center rounded-md bg-black text-xs font-bold text-white">
          C
        </div>
        <span className="text-sm font-semibold">Codex Switcher</span>
        <button
          className="ml-auto flex h-6 w-6 items-center justify-center rounded-md text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-900 disabled:opacity-50 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-100"
          onClick={() => void handleRefresh()}
          disabled={refreshing}
          title="Refresh usage"
        >
          <span className={`text-base leading-none ${refreshing ? "inline-block animate-spin" : ""}`}>
            ↻
          </span>
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-1.5">
        {loading ? (
          <div className="px-2 py-6 text-center text-xs text-gray-500 dark:text-gray-400">
            Loading...
          </div>
        ) : accounts.length === 0 ? (
          <div className="px-2 py-6 text-center text-xs text-gray-500 dark:text-gray-400">
            No accounts configured
          </div>
        ) : (
          accounts.map((account) => {
            const cached = cacheByAccountId.get(account.id);
            const plan = formatPlan(cached?.metadata?.plan_type ?? cached?.usage?.plan_type ?? null);
            const usage = cached?.usage;
            const stats = cached?.stats;
            const usageAge = formatCacheAge(cached?.usage_fetched_at ?? null);
            const statsAge = formatCacheAge(cached?.stats_fetched_at ?? null);
            const refreshError = refreshErrors[account.id];
            const windows =
              usage && !usage.error
                ? ([
                    {
                      label: "Session",
                      used: usage.primary_used_percent,
                      resetAt: usage.primary_resets_at,
                    },
                    {
                      label: "Weekly",
                      used: usage.secondary_used_percent,
                      resetAt: usage.secondary_resets_at,
                    },
                  ].filter((w) => w.used != null) as {
                    label: string;
                    used: number;
                    resetAt: number | null;
                  }[])
                : [];

            return (
              <button
                key={account.id}
                onClick={() => void handleSwitch(account)}
                disabled={switchingId !== null}
                className={`flex w-full items-start gap-2 rounded-lg px-2 py-1.5 text-left transition-colors disabled:opacity-60 ${
                  account.is_active
                    ? "bg-gray-100 dark:bg-gray-800"
                    : "hover:bg-gray-100 dark:hover:bg-gray-800"
                }`}
              >
                <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center">
                  {account.is_active && (
                    <svg
                      className="h-4 w-4 text-emerald-500"
                      viewBox="0 0 20 20"
                      fill="currentColor"
                    >
                      <path
                        fillRule="evenodd"
                        d="M16.7 5.3a1 1 0 010 1.4l-7.5 7.5a1 1 0 01-1.4 0L3.3 9.7a1 1 0 011.4-1.4l3.3 3.3 6.8-6.8a1 1 0 011.4 0z"
                        clipRule="evenodd"
                      />
                    </svg>
                  )}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1.5">
                    <span className="min-w-0 flex-1 truncate text-sm font-medium">
                      {account.name}
                    </span>
                    {plan && (
                      <span className="shrink-0 rounded bg-gray-200 px-1.5 py-0.5 text-[10px] font-medium text-gray-700 dark:bg-gray-700 dark:text-gray-200">
                        {plan}
                      </span>
                    )}
                  </span>
                  {windows.length > 0 ? (
                    <span className="mt-1.5 block space-y-1.5">
                      {windows.map((w) => {
                        const remaining = Math.max(0, 100 - w.used);
                        const tone = remainingTone(remaining);
                        const reset = formatResetAt(w.resetAt);
                        const exactReset = formatExactResetTime(w.resetAt, w.label === "Weekly");
                        return (
                          <span key={w.label} className="block">
                            <span className="flex items-center gap-1">
                              <span className="text-[11px] font-medium text-gray-700 dark:text-gray-200">
                                {w.label}
                              </span>
                              <span
                                className={`h-1.5 w-1.5 rounded-full ${tone.dot}`}
                              />
                            </span>
                            <span className="mt-0.5 block h-1.5 w-full overflow-hidden rounded-full bg-gray-200 dark:bg-gray-800">
                              <span
                                className={`block h-full rounded-full ${tone.bar}`}
                                style={{ width: `${Math.min(remaining, 100)}%` }}
                              />
                            </span>
                            <span className="mt-0.5 flex justify-between text-[11px] text-gray-500 dark:text-gray-400">
                              <span className={tone.text}>
                                {remaining.toFixed(0)}% left
                              </span>
                              {reset && (
                                <span className="shrink-0 whitespace-nowrap">
                                  {reset === "now" ? "Resets now" : `Resets in ${reset}`}
                                  {exactReset && ` • ${exactReset}`}
                                </span>
                              )}
                            </span>
                          </span>
                        );
                      })}
                    </span>
                  ) : usage?.error ? (
                    <span className="block truncate text-xs text-red-500 dark:text-red-400">
                      Usage unavailable
                    </span>
                  ) : account.email ? (
                    <span className="block truncate text-xs text-gray-500 dark:text-gray-400">
                      {account.email}
                    </span>
                  ) : null}
                  <span className={`mt-1 block truncate text-[10px] ${refreshError ? "text-red-500 dark:text-red-400" : "text-gray-400 dark:text-gray-500"}`}>
                    {refreshError
                      ? `Refresh failed: ${refreshError}`
                      : usage
                        ? `Cached usage · updated ${usageAge}`
                        : cached?.usage_fetched_at != null
                          ? `Usage expired · last refreshed ${usageAge}`
                          : "No cached usage · refresh manually"}
                  </span>
                  {account.is_active && stats?.available && (
                    <span className="mt-2 grid grid-cols-2 gap-1.5">
                      <span className="rounded-md bg-white px-2 py-1 text-[11px] text-gray-600 shadow-sm dark:bg-gray-950 dark:text-gray-300">
                        <span className="block font-medium text-gray-900 dark:text-gray-100">
                          {formatTokens(sumDailyTokens(stats, 1))}
                        </span>
                        <span>today</span>
                      </span>
                      <span className="rounded-md bg-white px-2 py-1 text-[11px] text-gray-600 shadow-sm dark:bg-gray-950 dark:text-gray-300">
                        <span className="block font-medium text-gray-900 dark:text-gray-100">
                          {formatTokens(sumDailyTokens(stats, 7))}
                        </span>
                        <span>last 7 days</span>
                      </span>
                    </span>
                  )}
                  {account.is_active && (
                    <span className="mt-1 block truncate text-[10px] text-gray-400 dark:text-gray-500">
                      {stats
                        ? `Cached stats · updated ${statsAge}`
                        : cached?.stats_fetched_at != null
                          ? `Stats expired · last refreshed ${statsAge}`
                          : "No cached stats · refresh manually"}
                    </span>
                  )}
                </span>
                {switchingId === account.id && (
                  <span className="shrink-0 text-xs text-gray-400">...</span>
                )}
              </button>
            );
          })
        )}
      </div>

      {(error ?? cacheError) && (
        <div className="border-t border-gray-100 px-3 py-2 text-xs text-red-600 dark:border-gray-800 dark:text-red-400">
          {error ?? cacheError}
        </div>
      )}

      {dockDisplayMode && (
        <div className="flex items-center gap-1 border-t border-gray-100 px-1.5 py-1.5 dark:border-gray-800">
          <span className="px-1.5 text-[11px] font-medium text-gray-500 dark:text-gray-400">
            Dock
          </span>
          <button
            onClick={() => void handleDockDisplayMode("show_in_dock")}
            className={`rounded-md px-2 py-1 text-[11px] font-semibold transition-colors ${
              dockDisplayMode === "show_in_dock"
                ? "bg-gray-900 text-white dark:bg-gray-100 dark:text-gray-900"
                : "bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
            }`}
          >
            Show
          </button>
          <button
            onClick={() => void handleDockDisplayMode("menu_bar_only")}
            className={`rounded-md px-2 py-1 text-[11px] font-semibold transition-colors ${
              dockDisplayMode === "menu_bar_only"
                ? "bg-gray-900 text-white dark:bg-gray-100 dark:text-gray-900"
                : "bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
            }`}
          >
            Menu Bar
          </button>
        </div>
      )}

      <div className="flex items-center gap-1 border-t border-gray-100 p-1.5 dark:border-gray-800">
        <button
          onClick={() => void invokeBackend("open_main_window")}
          className="flex-1 rounded-lg px-2 py-1.5 text-left text-sm transition-colors hover:bg-gray-100 dark:hover:bg-gray-800"
        >
          Open Codex Switcher
        </button>
        <button
          onClick={() => void invokeBackend("quit_app")}
          className="rounded-lg px-2 py-1.5 text-sm text-gray-500 transition-colors hover:bg-gray-100 hover:text-red-600 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-red-400"
        >
          Quit
        </button>
      </div>
    </div>
  );
}

export default TrayMenu;
