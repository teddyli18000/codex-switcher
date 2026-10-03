import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CachedAccountData } from "../types";
import {
  ACCOUNT_CACHE_CHANGED_EVENT,
  getCachedAccountDataMap,
  getNextAccountCacheExpiry,
  readCachedAccountData,
} from "../lib/accountCache";
import { isTauriRuntime } from "../lib/platform";

const CACHE_RECHECK_INTERVAL_MS = 30_000;

export function useCachedAccountData() {
  const [entries, setEntries] = useState<CachedAccountData[]>([]);
  const [now, setNow] = useState(() => Date.now());
  const [error, setError] = useState<string | null>(null);
  const requestSequence = useRef(0);

  const reload = useCallback(async () => {
    const requestId = ++requestSequence.current;
    try {
      const next = await readCachedAccountData();
      if (requestId === requestSequence.current) {
        setEntries(next);
        setNow(Date.now());
        setError(null);
      }
      return next;
    } catch (err) {
      if (requestId === requestSequence.current) {
        setError(err instanceof Error ? err.message : String(err));
      }
      throw err;
    }
  }, []);

  useEffect(() => {
    const expiryAt = getNextAccountCacheExpiry(entries, now);
    const untilExpiry = expiryAt === null ? CACHE_RECHECK_INTERVAL_MS : Math.max(0, expiryAt - now);
    const timer = window.setTimeout(() => {
      setNow(Date.now());
      void reload().catch((err) => {
        console.warn("Failed to reload cached account data:", err);
      });
    },
      Math.min(untilExpiry, CACHE_RECHECK_INTERVAL_MS),
    );
    return () => window.clearTimeout(timer);
  }, [entries, now, reload]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    const reloadLocalEventSnapshot = () => {
      void reload().catch((err) => {
        console.warn("Failed to reload cached account data:", err);
      });
    };
    const refreshVisibleSnapshot = () => {
      if (document.visibilityState === "hidden") return;
      setNow(Date.now());
      reloadLocalEventSnapshot();
    };

    window.addEventListener("focus", refreshVisibleSnapshot);
    document.addEventListener("visibilitychange", refreshVisibleSnapshot);
    window.addEventListener(ACCOUNT_CACHE_CHANGED_EVENT, reloadLocalEventSnapshot);

    if (isTauriRuntime()) {
      void import("@tauri-apps/api/event").then(({ listen }) =>
        listen("usage-cache-changed", () => {
          void reload().catch((err) => {
            console.warn("Failed to reload cached account data:", err);
          });
        }),
      ).then((stop) => {
        if (active) unlisten = stop;
        else stop();
      }).catch((err) => {
        console.warn("Failed to subscribe to cached account data changes:", err);
      });
    }

    return () => {
      active = false;
      window.removeEventListener("focus", refreshVisibleSnapshot);
      document.removeEventListener("visibilitychange", refreshVisibleSnapshot);
      window.removeEventListener(ACCOUNT_CACHE_CHANGED_EVENT, reloadLocalEventSnapshot);
      unlisten?.();
    };
  }, [reload]);

  const cacheByAccountId = useMemo(
    () => getCachedAccountDataMap(entries, now),
    [entries, now],
  );

  return { cacheByAccountId, entries, error, now, reload };
}
