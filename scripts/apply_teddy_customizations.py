from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace_exact(path: Path, old: str, new: str, count: int = 1) -> None:
    text = path.read_text(encoding="utf-8")
    found = text.count(old)
    if found != count:
        raise RuntimeError(f"{path}: expected {count} occurrence(s), found {found}")
    path.write_text(text.replace(old, new), encoding="utf-8")


def patch_manual_usage() -> None:
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

    path = ROOT / "src/TrayMenu.tsx"
    replace_exact(path, "      void loadUsage(list); // Don't block the list render on the usage calls.\n", "")
    replace_exact(path, "  }, [loadUsage]);\n\n  // Manual refresh", "  }, []);\n\n  // Manual refresh")

    path = ROOT / "src/App.tsx"
    replace_exact(
        path,
        """      const accountList = await loadAccounts();
      await refreshUsage(accountList);""",
        """      await loadAccounts();""",
    )

    path = ROOT / "src-tauri/src/tray.rs"
    text = path.read_text(encoding="utf-8")
    text = text.replace("    api::usage::get_account_usage,\n", "")
    text = text.replace(
        "    auth::{get_account, get_accounts_file, load_accounts, load_app_settings},\n",
        "    auth::{get_accounts_file, load_accounts, load_app_settings},\n",
    )
    text = text.replace("    poll_active_account_usage(app.clone());\n", "")
    old = '''
/// Poll the active account's usage so the tray title stays fresh even when the
/// main window's webview poller is hidden or suspended by the OS.
fn poll_active_account_usage<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || loop {
        let account = load_accounts()
            .ok()
            .and_then(|store| store.active_account_id)
            .and_then(|id| get_account(&id).ok().flatten());

        if let Some(account) = account {
            match tauri::async_runtime::block_on(get_account_usage(&account)) {
                // Keep the last known title on transient fetch errors.
                Ok(usage) => ingest_usage(&app, vec![usage]),
                Err(error) => eprintln!("Failed to poll usage for tray title: {error}"),
            }
        }

        std::thread::sleep(Duration::from_secs(60));
    });
}
'''
    if old not in text:
        raise RuntimeError("native tray poller block not found")
    path.write_text(text.replace(old, "\n"), encoding="utf-8")


def patch_reset_credit_backend() -> None:
    path = ROOT / "src-tauri/src/api/usage.rs"
    replace_exact(path, "use chrono::{DateTime, Utc};", "use chrono::{DateTime, Local, Utc};")
    insert = r'''
const CHATGPT_RESET_CREDITS_API: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
'''
    replace_exact(path, "const CHATGPT_CODEX_RESPONSES_API: &str = \"https://chatgpt.com/backend-api/codex/responses\";\n", "const CHATGPT_CODEX_RESPONSES_API: &str = \"https://chatgpt.com/backend-api/codex/responses\";\n" + insert)
    reset_code = r'''
/// Fetch and format Codex reset credit information for one ChatGPT account.
pub async fn get_account_reset_credits(account: &StoredAccount) -> Result<String> {
    println!("[ResetCredits] Fetching reset credits for account: {}", account.name);

    match &account.auth_data {
        AuthData::ApiKey { .. } => anyhow::bail!("Reset credit info is not available for API key accounts"),
        AuthData::ChatGPT { .. } => get_reset_credits_with_chatgpt_auth(account).await,
    }
}

async fn get_reset_credits_with_chatgpt_auth(account: &StoredAccount) -> Result<String> {
    let fresh_account = ensure_chatgpt_tokens_fresh(account).await?;
    let (access_token, chatgpt_account_id) = extract_chatgpt_auth(&fresh_account)?;
    let response = send_chatgpt_reset_credits_request(access_token, chatgpt_account_id).await?;

    if response.status() == StatusCode::UNAUTHORIZED {
        println!(
            "[ResetCredits] Unauthorized for account {}, refreshing token and retrying once",
            fresh_account.name
        );
        let refreshed_account = refresh_chatgpt_tokens(&fresh_account).await?;
        let (retry_token, retry_account_id) = extract_chatgpt_auth(&refreshed_account)?;
        let retry_response = send_chatgpt_reset_credits_request(retry_token, retry_account_id).await?;
        return parse_reset_credits_response(retry_response).await;
    }

    parse_reset_credits_response(response).await
}

async fn send_chatgpt_reset_credits_request(
    access_token: &str,
    chatgpt_account_id: Option<&str>,
) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CODEX_USER_AGENT));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}")).context("Invalid access token")?,
    );

    if let Some(account_id) = chatgpt_account_id {
        if let Ok(header_name) = HeaderName::from_bytes(b"OpenAI-Account") {
            headers.insert(
                header_name,
                HeaderValue::from_str(account_id).context("Invalid ChatGPT account id")?,
            );
        }
    }

    println!("[ResetCredits] Requesting: {CHATGPT_RESET_CREDITS_API}");
    client
        .get(CHATGPT_RESET_CREDITS_API)
        .headers(headers)
        .send()
        .await
        .context("Failed to send reset credit request")
}

async fn parse_reset_credits_response(response: reqwest::Response) -> Result<String> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED {
            anyhow::bail!("HTTP 401: token may be expired. Re-login this account and try again.");
        }
        if status == StatusCode::FORBIDDEN {
            anyhow::bail!("HTTP 403: this account may not have access to reset credit info, or the account id did not match.");
        }
        if status == StatusCode::NOT_FOUND {
            anyhow::bail!("HTTP 404: reset credit endpoint may have changed.");
        }
        anyhow::bail!("Reset credit API returned HTTP {status}. {}", truncate_text(&body, 500));
    }

    let body_text = response
        .text()
        .await
        .context("Failed to read reset credit response body")?;
    let payload: Value = serde_json::from_str(&body_text)
        .context("Failed to parse reset credit response JSON")?;
    let object = payload
        .as_object()
        .context("Reset credit response top level was not an object")?;

    Ok(format_reset_credit_output(object))
}

fn format_reset_credit_output(data: &serde_json::Map<String, Value>) -> String {
    let available = find_first_value(
        data,
        &[
            "available_reset_credits",
            "availableResetCredits",
            "available_count",
            "availableCount",
            "available",
        ],
    );
    let total_earned = find_first_value(
        data,
        &[
            "total_earned_count",
            "totalEarnedCount",
            "total_earned",
            "totalEarned",
        ],
    );

    let mut lines = vec![
        "Codex reset credits".to_string(),
        "========================".to_string(),
        format!("Available reset credits: {}", display_count(available)),
        format!("Total earned count:       {}", display_count(total_earned)),
    ];

    if let Some(credits) = find_credit_items(data) {
        lines.push(String::new());
        lines.push("Expiries:".to_string());
        for (index, credit) in credits.iter().enumerate() {
            let expiry = credit.as_object().and_then(|item| {
                find_first_value(
                    item,
                    &[
                        "expires_at",
                        "expiresAt",
                        "expiration_time",
                        "expirationTime",
                        "expiry",
                    ],
                )
            });
            lines.push(format!("  {}. {}", index + 1, format_time_local(expiry)));
        }
    } else {
        lines.push("Credit expiries:          not found".to_string());
    }

    lines.join("\n")
}

fn find_first_value<'a>(
    data: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    keys.iter().find_map(|key| data.get(*key))
}

fn find_credit_items<'a>(data: &'a serde_json::Map<String, Value>) -> Option<&'a Vec<Value>> {
    ["reset_credits", "resetCredits", "credits", "items"]
        .iter()
        .find_map(|key| data.get(*key).and_then(Value::as_array))
}

fn display_count(value: Option<&Value>) -> String {
    match value {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) if !text.trim().is_empty() => text.trim().to_string(),
        _ => "unknown".to_string(),
    }
}

fn format_time_local(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "unknown".to_string();
    };

    let parsed_utc = match value {
        Value::Number(number) => {
            let Some(raw) = number.as_f64() else {
                return "unknown".to_string();
            };
            let seconds = if raw > 10_000_000_000.0 { raw / 1000.0 } else { raw };
            DateTime::<Utc>::from_timestamp(seconds as i64, 0)
        }
        Value::String(text) => {
            let normalized = text.trim().replace('Z', "+00:00");
            DateTime::parse_from_rfc3339(&normalized)
                .ok()
                .map(|value| value.with_timezone(&Utc))
        }
        _ => None,
    };

    parsed_utc
        .map(|value| value.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S UTC%:z").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
'''
    replace_exact(path, "/// Refresh all account usage\n", reset_code + "\n/// Refresh all account usage\n")

    path = ROOT / "src-tauri/src/commands/usage.rs"
    replace_exact(path, "    fetch_chatgpt_account_metadata, get_account_usage, refresh_all_usage,\n", "    fetch_chatgpt_account_metadata, get_account_reset_credits, get_account_usage, refresh_all_usage,\n")
    command = r'''
/// Get Codex reset credit details for a specific account.
#[tauri::command]
pub async fn get_reset_credits(account_id: String) -> Result<String, String> {
    let account = get_account(&account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    get_account_reset_credits(&account)
        .await
        .map_err(|e| e.to_string())
}

'''
    replace_exact(path, "/// Force-refresh account metadata", command + "/// Force-refresh account metadata")

    path = ROOT / "src-tauri/src/lib.rs"
    replace_exact(path, "     get_masked_account_ids, get_usage, hide_tray_window, import_accounts_full_encrypted_file,\n", "     get_masked_account_ids, get_reset_credits, get_usage, hide_tray_window, import_accounts_full_encrypted_file,\n")
    replace_exact(path, "            get_usage,\n            refresh_account_metadata,", "            get_usage,\n            get_reset_credits,\n            refresh_account_metadata,")


def patch_reset_credit_frontend() -> None:
    path = ROOT / "src/components/AccountCard.tsx"
    replace_exact(path, "import type { AccountWithUsage } from \"../types\";\n", "import type { AccountWithUsage } from \"../types\";\nimport { invokeBackend } from \"../lib/platform\";\n")
    replace_exact(path, "  const [isEditing, setIsEditing] = useState(false);\n", "  const [isEditing, setIsEditing] = useState(false);\n  const [isFetchingResetCredits, setIsFetchingResetCredits] = useState(false);\n  const [resetCreditOutput, setResetCreditOutput] = useState<string | null>(null);\n  const [resetCreditError, setResetCreditError] = useState<string | null>(null);\n")
    handler = r'''
  const handleResetCredits = async () => {
    setIsFetchingResetCredits(true);
    setResetCreditError(null);
    try {
      const output = await invokeBackend<string>("get_reset_credits", {
        accountId: account.id,
      });
      setResetCreditOutput(output);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setResetCreditError(message);
      setResetCreditOutput(`Error: ${message}`);
    } finally {
      setIsFetchingResetCredits(false);
    }
  };

'''
    replace_exact(path, "  const handleRename = async () => {\n", handler + "  const handleRename = async () => {\n")
    reset_button = r'''        <button
          onClick={() => {
            void handleResetCredits();
          }}
          disabled={isFetchingResetCredits || account.auth_mode !== "chat_g_p_t"}
          className={`px-3 py-2 text-xs font-medium rounded-lg transition-colors whitespace-nowrap ${
            isFetchingResetCredits
              ? "bg-violet-100 dark:bg-violet-900/30 text-violet-500 dark:text-violet-300"
              : "bg-violet-50 dark:bg-violet-900/20 hover:bg-violet-100 dark:hover:bg-violet-900/40 text-violet-700 dark:text-violet-300"
          } disabled:opacity-50`}
          title={account.auth_mode === "chat_g_p_t" ? "Fetch Codex reset card count and expiries" : "Reset cards are only available for ChatGPT accounts"}
        >
          {isFetchingResetCredits ? "Cards..." : "Cards"}
        </button>
'''
    replace_exact(path, "        <button\n          onClick={handleRefresh}\n", reset_button + "        <button\n          onClick={handleRefresh}\n")
    output_block = r'''

      {resetCreditOutput && (
        <div
          className={`mt-3 overflow-hidden rounded-xl border ${
            resetCreditError
              ? "border-red-200 bg-red-50 dark:border-red-900/60 dark:bg-red-950/30"
              : "border-violet-200 bg-violet-50/70 dark:border-violet-900/60 dark:bg-violet-950/30"
          }`}
        >
          <div className="flex items-center justify-between gap-2 border-b border-black/5 px-3 py-2 dark:border-white/10">
            <span className={`text-xs font-semibold ${resetCreditError ? "text-red-700 dark:text-red-300" : "text-violet-700 dark:text-violet-300"}`}>
              Reset cards
            </span>
            <button
              onClick={() => {
                setResetCreditOutput(null);
                setResetCreditError(null);
              }}
              className="rounded-md px-2 py-0.5 text-xs text-gray-500 transition-colors hover:bg-black/5 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-white/10 dark:hover:text-gray-200"
            >
              Hide
            </button>
          </div>
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words px-3 py-2 font-mono text-xs leading-5 text-gray-700 dark:text-gray-200">
            {resetCreditOutput}
          </pre>
        </div>
      )}
'''
    replace_exact(path, "      </div>\n    </div>\n  );\n}", "      </div>" + output_block + "\n    </div>\n  );\n}")


def verify() -> None:
    accounts = (ROOT / "src/hooks/useAccounts.ts").read_text(encoding="utf-8")
    tray_ui = (ROOT / "src/TrayMenu.tsx").read_text(encoding="utf-8")
    app = (ROOT / "src/App.tsx").read_text(encoding="utf-8")
    tray_rs = (ROOT / "src-tauri/src/tray.rs").read_text(encoding="utf-8")
    account_card = (ROOT / "src/components/AccountCard.tsx").read_text(encoding="utf-8")
    usage_api = (ROOT / "src-tauri/src/api/usage.rs").read_text(encoding="utf-8")

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
        "get_reset_credits" in account_card,
        "CHATGPT_RESET_CREDITS_API" in usage_api,
        "OpenAI-Account" in usage_api,
    ]
    if not all(required):
        raise RuntimeError("required manual usage, warm-up, or reset credit behavior missing")


if __name__ == "__main__":
    patch_manual_usage()
    patch_reset_credit_backend()
    patch_reset_credit_frontend()
    verify()
