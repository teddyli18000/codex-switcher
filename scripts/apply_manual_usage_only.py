from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.2.4"


def replace_exact(path: Path, old: str, new: str, count: int = 1) -> None:
    text = path.read_text(encoding="utf-8")
    found = text.count(old)
    if found != count:
        raise RuntimeError(f"{path}: expected {count} matches, found {found}")
    path.write_text(text.replace(old, new), encoding="utf-8")


def patch_accounts_hook() -> None:
    path = ROOT / "src/hooks/useAccounts.ts"
    replace_exact(
        path,
        """        const accountList = await loadAccounts();
        await refreshUsage(accountList);""",
        """        await loadAccounts();""",
        4,
    )
    replace_exact(path, "    [loadAccounts, refreshUsage]", "    [loadAccounts]", 3)
    replace_exact(path, "  }, [loadAccounts, refreshUsage]);", "  }, [loadAccounts]);", 1)
    replace_exact(
        path,
        """  useEffect(() => {
    loadAccounts().then((accountList) => refreshUsage(accountList));
    
    // Auto-refresh usage every 60 seconds (same as official Codex CLI)
    const interval = setInterval(() => {
      refreshUsage().catch(() => {});
    }, 60000);
    
    return () => clearInterval(interval);
  }, [loadAccounts, refreshUsage]);""",
        """  useEffect(() => {
    void loadAccounts();
  }, [loadAccounts]);""",
    )


def patch_tray_ui() -> None:
    path = ROOT / "src/TrayMenu.tsx"
    replace_exact(path, "      void loadUsage(list); // Don't block the list render on the usage calls.\n", "")
    replace_exact(path, "  }, [loadUsage]);\n\n  // Manual refresh", "  }, []);\n\n  // Manual refresh")


def patch_main_ui() -> None:
    path = ROOT / "src/App.tsx"
    replace_exact(
        path,
        """      const accountList = await loadAccounts();
      await refreshUsage(accountList);""",
        """      await loadAccounts();""",
    )


def patch_native_tray() -> None:
    path = ROOT / "src-tauri/src/tray.rs"
    text = path.read_text(encoding="utf-8")
    text = text.replace("    api::usage::get_account_usage,\n", "")
    text = text.replace(
        "    auth::{get_account, get_accounts_file, load_accounts, load_app_settings},\n",
        "    auth::{get_accounts_file, load_accounts, load_app_settings},\n",
    )
    text = text.replace("    poll_active_account_usage(app.clone());\n", "")
    pattern = re.compile(
        r"\n/// Poll the active account's usage so the tray title stays fresh even when the\n"
        r"/// main window's webview poller is hidden or suspended by the OS\.\n"
        r"fn poll_active_account_usage<R: Runtime>\(app: AppHandle<R>\) \{.*?\n\}\n",
        re.S,
    )
    text, matches = pattern.subn("\n", text, count=1)
    if matches != 1:
        raise RuntimeError(f"{path}: native polling function not found")
    path.write_text(text, encoding="utf-8")


def patch_distribution_config() -> None:
    package_path = ROOT / "package.json"
    package = json.loads(package_path.read_text(encoding="utf-8"))
    package["version"] = VERSION
    package_path.write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")

    cargo_path = ROOT / "src-tauri/Cargo.toml"
    replace_exact(cargo_path, 'version = "0.2.3"', f'version = "{VERSION}"')

    lock_path = ROOT / "src-tauri/Cargo.lock"
    replace_exact(
        lock_path,
        'name = "codex-switcher"\nversion = "0.2.3"',
        f'name = "codex-switcher"\nversion = "{VERSION}"',
    )

    tauri_path = ROOT / "src-tauri/tauri.conf.json"
    config = json.loads(tauri_path.read_text(encoding="utf-8"))
    config["version"] = VERSION
    config["bundle"]["createUpdaterArtifacts"] = False
    config["plugins"]["updater"]["endpoints"] = [
        "https://github.com/teddyli18000/codex-switcher/releases/latest/download/latest.json"
    ]
    tauri_path.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")


def verify() -> None:
    accounts = (ROOT / "src/hooks/useAccounts.ts").read_text(encoding="utf-8")
    tray_ui = (ROOT / "src/TrayMenu.tsx").read_text(encoding="utf-8")
    app = (ROOT / "src/App.tsx").read_text(encoding="utf-8")
    tray_rs = (ROOT / "src-tauri/src/tray.rs").read_text(encoding="utf-8")

    forbidden = [
        "refreshUsage().catch(() => {})" in accounts,
        "loadAccounts().then((accountList) => refreshUsage(accountList))" in accounts,
        "void loadUsage(list); // Don't block" in tray_ui,
        "poll_active_account_usage" in tray_rs,
        "await refreshUsage(accountList);" in app,
    ]
    if any(forbidden):
        raise RuntimeError("automatic usage query path remains")

    required = [
        "await refreshUsage(undefined, { refreshMetadata: true })" in app,
        "await loadUsage(list);" in tray_ui,
        "const refreshSingleUsage" in accounts,
        "const warmupAccount" in accounts,
        "const warmupAllAccounts" in accounts,
        "runAutoWarmupForAccount" in app,
    ]
    if not all(required):
        raise RuntimeError("manual refresh or warm-up behavior was lost")

    package = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
    tauri_config = json.loads((ROOT / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))
    cargo = (ROOT / "src-tauri/Cargo.toml").read_text(encoding="utf-8")
    cargo_lock = (ROOT / "src-tauri/Cargo.lock").read_text(encoding="utf-8")
    if package["version"] != VERSION or tauri_config["version"] != VERSION:
        raise RuntimeError("custom build version is inconsistent")
    if f'version = "{VERSION}"' not in cargo:
        raise RuntimeError("Cargo package version was not updated")
    if f'name = "codex-switcher"\nversion = "{VERSION}"' not in cargo_lock:
        raise RuntimeError("Cargo lock version was not updated")
    if tauri_config["bundle"]["createUpdaterArtifacts"] is not False:
        raise RuntimeError("updater artifacts must be disabled for the custom build")


if __name__ == "__main__":
    patch_accounts_hook()
    patch_tray_ui()
    patch_main_ui()
    patch_native_tray()
    patch_distribution_config()
    verify()
