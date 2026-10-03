import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type {
  AccountInfo,
  UsageInfo,
  AccountWithUsage,
  WarmupSummary,
  ImportAccountsSummary,
} from "../types";
import { invokeBackend, isTauriRuntime, type FileSource } from "../lib/platform";
import { useCachedAccountData } from "./useCachedAccountData";

interface AccountRefreshState {
  usageLoading: boolean;
  error: string | null;
}

const maxConcurrentUsageRequests = 10;

export function useAccounts() {
  const [accountList, setAccountList] = useState<AccountInfo[]>([]);
  const [refreshState, setRefreshState] = useState<Record<string, AccountRefreshState>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const accountsRef = useRef<AccountWithUsage[]>([]);
  const metadataRefreshInFlightRef = useRef(new Set<string>());
  const usageRefreshInFlightRef = useRef(new Set<string>());
  const { cacheByAccountId, error: cacheError, reload: reloadCache } =
    useCachedAccountData();

  const accounts = useMemo(
    () =>
      accountList.map((account) => {
        const cache = cacheByAccountId.get(account.id);
        const refresh = refreshState[account.id];
        return {
          ...account,
          plan_type: cache?.metadata?.plan_type ?? cache?.usage?.plan_type ?? null,
          subscription_expires_at:
            cache?.metadata?.subscription_expires_at ?? null,
          usage: cache?.usage ?? undefined,
          usageFetchedAt: cache?.usage_fetched_at ?? null,
          usageLoading: refresh?.usageLoading ?? false,
          usageRefreshError: refresh?.error ?? null,
          stats: cache?.stats ?? null,
          statsFetchedAt: cache?.stats_fetched_at ?? null,
          metadataFetchedAt: cache?.metadata_fetched_at ?? null,
        };
      }),
    [accountList, cacheByAccountId, refreshState],
  );

  useEffect(() => {
    accountsRef.current = accounts;
  }, [accounts]);

  const buildUsageError = useCallback(
    (accountId: string, message: string, planType: string | null): UsageInfo => ({
      account_id: accountId,
      plan_type: planType,
      primary_used_percent: null,
      primary_window_minutes: null,
      primary_resets_at: null,
      secondary_used_percent: null,
      secondary_window_minutes: null,
      secondary_resets_at: null,
      has_credits: null,
      unlimited_credits: null,
      credits_balance: null,
      error: message,
    }),
    [],
  );

  const runWithConcurrency = useCallback(
    async <T,>(items: T[], worker: (item: T) => Promise<void>, concurrency: number) => {
      if (items.length === 0) return;
      const limit = Math.min(Math.max(concurrency, 1), items.length);
      let index = 0;
      const runners = Array.from({ length: limit }, async () => {
        while (true) {
          const current = index++;
          if (current >= items.length) return;
          await worker(items[current]);
        }
      });
      await Promise.allSettled(runners);
    },
    [],
  );

  const loadAccounts = useCallback(async (_preserveUsage = false) => {
    try {
      setLoading(true);
      setError(null);
      const list = await invokeBackend<AccountInfo[]>("list_accounts");
      setAccountList(list);
      setRefreshState((prev) =>
        Object.fromEntries(
          Object.entries(prev).filter(([accountId]) =>
            list.some((account) => account.id === accountId),
          ),
        ),
      );
      try {
        await reloadCache();
      } catch (cacheReadError) {
        console.warn("Failed to load cached account data:", cacheReadError);
      }
      return list;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return [];
    } finally {
      setLoading(false);
    }
  }, [reloadCache]);

  const refreshMetadata = useCallback(
    async (list: AccountInfo[] | AccountWithUsage[]) => {
      const dueAccounts = list.filter(
        (account) => !metadataRefreshInFlightRef.current.has(account.id),
      );
      dueAccounts.forEach((account) => {
        metadataRefreshInFlightRef.current.add(account.id);
      });

      const errors = new Map<string, string>();
      await runWithConcurrency(
        dueAccounts,
        async (account) => {
          try {
            await invokeBackend<AccountInfo>("refresh_account_metadata", {
              accountId: account.id,
            });
          } catch (err) {
            errors.set(account.id, err instanceof Error ? err.message : String(err));
          } finally {
            metadataRefreshInFlightRef.current.delete(account.id);
          }
        },
        maxConcurrentUsageRequests,
      );
      return errors;
    },
    [runWithConcurrency],
  );

  const refreshUsage = useCallback(
    async (
      accountList?: AccountInfo[] | AccountWithUsage[],
      options?: { refreshMetadata?: boolean },
    ) => {
      const requested = accountList ?? accountsRef.current;
      const alreadyRefreshing = requested.filter((account) =>
        usageRefreshInFlightRef.current.has(account.id),
      );
      const list = requested.filter(
        (account) => !usageRefreshInFlightRef.current.has(account.id),
      );
      if (list.length === 0) {
        if (alreadyRefreshing.length > 0) {
          throw new Error("A usage refresh is already in progress.");
        }
        return new Map<string, UsageInfo>();
      }

      const accountById = new Map(accountsRef.current.map((account) => [account.id, account]));
      const ids = list.map((account) => account.id);
      ids.forEach((accountId) => usageRefreshInFlightRef.current.add(accountId));
      setRefreshState((prev) => {
        const next = { ...prev };
        for (const accountId of ids) {
          next[accountId] = { usageLoading: true, error: null };
        }
        return next;
      });

      const usageResults = new Map<string, UsageInfo>();
      const errors = new Map<string, string>();
      const usagePromise = runWithConcurrency(
        list,
        async (account) => {
          try {
            const usage = await invokeBackend<UsageInfo>("get_usage", {
              accountId: account.id,
            });
            usageResults.set(account.id, usage);
            if (usage.error) errors.set(account.id, usage.error);
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            usageResults.set(
              account.id,
              buildUsageError(account.id, message, accountById.get(account.id)?.plan_type ?? account.plan_type ?? null),
            );
            errors.set(account.id, message);
          }
        },
        maxConcurrentUsageRequests,
      );
      const metadataPromise = options?.refreshMetadata
        ? refreshMetadata(list)
        : Promise.resolve(new Map<string, string>());

      let metadataErrors = new Map<string, string>();
      let cacheReadError: string | null = null;
      try {
        await Promise.all([usagePromise, metadataPromise.then((result) => { metadataErrors = result; })]);
      } finally {
        try {
          await reloadCache();
        } catch (err) {
          cacheReadError = err instanceof Error ? err.message : String(err);
        }
        setRefreshState((prev) => {
          const next = { ...prev };
          for (const accountId of ids) {
            next[accountId] = {
              usageLoading: false,
              error:
                errors.get(accountId) ??
                metadataErrors.get(accountId) ??
                cacheReadError,
            };
          }
          return next;
        });
        ids.forEach((accountId) => usageRefreshInFlightRef.current.delete(accountId));
      }
      const failureMessages = [
        ...new Set([
          ...errors.values(),
          ...metadataErrors.values(),
          ...(cacheReadError ? [cacheReadError] : []),
          ...(alreadyRefreshing.length > 0 ? ["Some accounts are already refreshing."] : []),
        ]),
      ];
      if (failureMessages.length > 0) throw new Error(failureMessages.join("; "));
      return usageResults;
    },
    [buildUsageError, refreshMetadata, reloadCache, runWithConcurrency],
  );

  const refreshSingleUsage = useCallback(
    async (accountId: string, options?: { refreshMetadata?: boolean }) => {
      const account = accountsRef.current.find((item) => item.id === accountId);
      if (!account) throw new Error("Account is no longer available.");
      const results = await refreshUsage([account], options);
      const usage = results.get(accountId);
      if (!usage || usage.error) {
        throw new Error(usage?.error ?? "Usage refresh did not return data.");
      }
      return usage;
    },
    [refreshUsage],
  );

  const warmupAccount = useCallback(async (accountId: string) => {
    try {
      await invokeBackend("warmup_account", { accountId });
      await reloadCache();
    } catch (err) {
      console.error("Failed to warm up account:", err);
      throw err;
    }
  }, [reloadCache]);

  const warmupAllAccounts = useCallback(async () => {
    try {
      const summary = await invokeBackend<WarmupSummary>("warmup_all_accounts");
      await reloadCache();
      return summary;
    } catch (err) {
      console.error("Failed to warm up all accounts:", err);
      throw err;
    }
  }, [reloadCache]);

  const switchAccount = useCallback(
    async (accountId: string) => {
      await invokeBackend("switch_account", { accountId });
      await loadAccounts(true);
    },
    [loadAccounts],
  );

  const deleteAccount = useCallback(
    async (accountId: string) => {
      await invokeBackend("delete_account", { accountId });
      await loadAccounts(true);
    },
    [loadAccounts],
  );

  const renameAccount = useCallback(
    async (accountId: string, newName: string) => {
      await invokeBackend("rename_account", { accountId, newName });
      await loadAccounts(true);
    },
    [loadAccounts],
  );

  const importFromFile = useCallback(
    async (source: FileSource, name: string) => {
      if (typeof source === "string") {
        await invokeBackend<AccountInfo>("add_account_from_file", { path: source, name });
      } else {
        const contents = await source.text();
        await invokeBackend<AccountInfo>("add_account_from_auth_json_text", {
          name,
          contents,
        });
      }
      await loadAccounts();
    },
    [loadAccounts],
  );

  const startOAuthLogin = useCallback(async (accountName: string) => {
    return invokeBackend<{ auth_url: string; callback_port: number }>("start_login", {
      accountName,
    });
  }, []);

  const completeOAuthLogin = useCallback(async () => {
    const account = await invokeBackend<AccountInfo>("complete_login");
    await loadAccounts();
    return account;
  }, [loadAccounts]);

  const exportAccountsSlimText = useCallback(async () => {
    return invokeBackend<string>("export_accounts_slim_text");
  }, []);

  const importAccountsSlimText = useCallback(
    async (payload: string) => {
      const summary = await invokeBackend<ImportAccountsSummary>("import_accounts_slim_text", {
        payload,
      });
      await loadAccounts();
      return summary;
    },
    [loadAccounts],
  );

  const exportAccountsFullEncryptedFile = useCallback(async (path: string) => {
    await invokeBackend("export_accounts_full_encrypted_file", { path });
  }, []);

  const importAccountsFullEncryptedFile = useCallback(
    async (path: string) => {
      const summary = await invokeBackend<ImportAccountsSummary>(
        "import_accounts_full_encrypted_file",
        { path },
      );
      await loadAccounts();
      return summary;
    },
    [loadAccounts],
  );

  const cancelOAuthLogin = useCallback(async () => {
    try {
      await invokeBackend("cancel_login");
    } catch (err) {
      console.error("Failed to cancel login:", err);
    }
  }, []);

  const loadMaskedAccountIds = useCallback(async () => {
    try {
      return await invokeBackend<string[]>("get_masked_account_ids");
    } catch (err) {
      console.error("Failed to load masked account IDs:", err);
      return [];
    }
  }, []);

  const saveMaskedAccountIds = useCallback(async (ids: string[]) => {
    try {
      await invokeBackend("set_masked_account_ids", { ids });
    } catch (err) {
      console.error("Failed to save masked account IDs:", err);
    }
  }, []);

  useEffect(() => {
    void loadAccounts();
  }, [loadAccounts]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    void (async () => {
      if (!isTauriRuntime()) return;
      const { listen } = await import("@tauri-apps/api/event");
      const stop = await listen("accounts-changed", () => {
        void loadAccounts(true);
      });
      if (active) unlisten = stop;
      else stop();
    })().catch((err) => {
      console.warn("Failed to subscribe to account changes:", err);
    });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [loadAccounts]);

  return {
    accounts,
    loading,
    error: error ?? cacheError,
    loadAccounts,
    refreshUsage,
    refreshSingleUsage,
    warmupAccount,
    warmupAllAccounts,
    switchAccount,
    deleteAccount,
    renameAccount,
    importFromFile,
    exportAccountsSlimText,
    importAccountsSlimText,
    exportAccountsFullEncryptedFile,
    importAccountsFullEncryptedFile,
    startOAuthLogin,
    completeOAuthLogin,
    cancelOAuthLogin,
    loadMaskedAccountIds,
    saveMaskedAccountIds,
  };
}
