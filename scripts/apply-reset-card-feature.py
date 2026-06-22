from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8").replace("\r\n", "\n")


def write(path: str, text: str) -> None:
    (ROOT / path).write_text(text, encoding="utf-8", newline="\n")


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected 1 occurrence, found {count}")
    return text.replace(old, new, 1)


def patch_manual_usage() -> None:
    hook_path = "src/hooks/useAccounts.ts"
    hook = read(hook_path)
    hook = re.sub(
        r"\n\s*const accountList = await loadAccounts\(\);\n\s*await refreshUsage\(accountList\);",
        "\n        await loadAccounts();",
        hook,
    )
    hook = hook.replace("    [loadAccounts, refreshUsage]", "    [loadAccounts]")
    hook = hook.replace("  }, [loadAccounts, refreshUsage]);", "  }, [loadAccounts]);")
    hook = re.sub(
        r"  useEffect\(\(\) => \{\n    loadAccounts\(\)\.then\(\(accountList\) => refreshUsage\(accountList\)\);\n\s*\n    // Auto-refresh usage every 60 seconds \(same as official Codex CLI\)\n    const interval = setInterval\(\(\) => \{\n      refreshUsage\(\)\.catch\(\(\) => \{\}\);\n    \}, 60000\);\n\s*\n    return \(\) => clearInterval\(interval\);\n  \}, \[loadAccounts, refreshUsage\]\);",
        "  useEffect(() => {\n    void loadAccounts();\n  }, [loadAccounts]);",
        hook,
        count=1,
    )
    write(hook_path, hook)

    app_path = "src/App.tsx"
    app = read(app_path)
    app = re.sub(
        r"\n\s*const accountList = await loadAccounts\(\);\n\s*await refreshUsage\(accountList\);",
        "\n      await loadAccounts();",
        app,
        count=1,
    )
    write(app_path, app)

    verify_manual_usage()


def patch_backend() -> None:
    api_path = "src-tauri/src/api/usage.rs"
    usage = read(api_path)
    usage = replace_once(
        usage,
        'const CHATGPT_CODEX_RESPONSES_API: &str = "https://chatgpt.com/backend-api/codex/responses";\n',
        'const CHATGPT_CODEX_RESPONSES_API: &str = "https://chatgpt.com/backend-api/codex/responses";\nconst CHATGPT_RESET_CARDS_API: &str =\n    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";\n',
        "reset card endpoint constant",
    )
    backend_block = r'''
/// Fetch Codex reset-card information and return terminal-style output.
pub async fn get_account_reset_cards_terminal_output(account: &StoredAccount) -> Result<String> {
    match &account.auth_data {
        AuthData::ApiKey { .. } => Ok(format!(
            "Codex reset card lookup\n=======================\nAccount: {}\nEmail: {}\nAuth mode: API key\n\nReset card lookup is only available for ChatGPT OAuth accounts.",
            account.name,
            account.email.as_deref().unwrap_or("unknown")
        )),
        AuthData::ChatGPT { .. } => get_reset_cards_with_chatgpt_auth(account).await,
    }
}

async fn get_reset_cards_with_chatgpt_auth(account: &StoredAccount) -> Result<String> {
    let fresh_account = ensure_chatgpt_tokens_fresh(account).await?;
    let (access_token, chatgpt_account_id) = extract_chatgpt_auth(&fresh_account)?;

    let response = send_chatgpt_reset_cards_request(access_token, chatgpt_account_id).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        let refreshed_account = refresh_chatgpt_tokens(&fresh_account).await?;
        let (retry_token, retry_account_id) = extract_chatgpt_auth(&refreshed_account)?;
        let retry_response = send_chatgpt_reset_cards_request(retry_token, retry_account_id).await?;
        return format_reset_cards_terminal_output(&refreshed_account, retry_response, true).await;
    }

    format_reset_cards_terminal_output(&fresh_account, response, false).await
}

async fn send_chatgpt_reset_cards_request(
    access_token: &str,
    chatgpt_account_id: Option<&str>,
) -> Result<reqwest::Response> {
    let client = reqwest::Client::new();
    let mut headers = build_chatgpt_headers(access_token, chatgpt_account_id)?;

    // Some ChatGPT backend endpoints accept this account routing header. Keep the
    // existing chatgpt-account-id header too so the known-good usage flow is not disturbed.
    if let Some(account_id) = chatgpt_account_id {
        if let Ok(header_name) = HeaderName::from_bytes(b"OpenAI-Account") {
            headers.insert(
                header_name,
                HeaderValue::from_str(account_id).context("Invalid ChatGPT account id")?,
            );
        }
    }

    client
        .get(CHATGPT_RESET_CARDS_API)
        .headers(headers)
        .send()
        .await
        .context("Failed to send reset card request")
}

async fn format_reset_cards_terminal_output(
    account: &StoredAccount,
    response: reqwest::Response,
    retried_after_refresh: bool,
) -> Result<String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read reset card response body")?;
    let pretty_body = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| body.clone());

    let mut output = String::new();
    output.push_str("Codex reset card lookup\n");
    output.push_str("=======================\n");
    output.push_str(&format!("Account: {}\n", account.name));
    output.push_str(&format!(
        "Email: {}\n",
        account.email.as_deref().unwrap_or("unknown")
    ));
    output.push_str(&format!("Endpoint: {}\n", CHATGPT_RESET_CARDS_API));
    output.push_str(&format!("HTTP status: {}\n", status));
    output.push_str(&format!(
        "Retried after token refresh: {}\n",
        if retried_after_refresh { "yes" } else { "no" }
    ));
    output.push_str(&format!("Fetched at: {}\n", chrono::Utc::now().to_rfc3339()));
    output.push_str("\n--- Complete response body ---\n");
    if pretty_body.trim().is_empty() {
        output.push_str("<empty response body>\n");
    } else {
        output.push_str(&pretty_body);
        if !pretty_body.ends_with('\n') {
            output.push('\n');
        }
    }

    Ok(output)
}

'''
    usage = replace_once(
        usage,
        "/// Refresh all account usage\npub async fn refresh_all_usage",
        backend_block + "/// Refresh all account usage\npub async fn refresh_all_usage",
        "reset card API implementation insertion point",
    )
    write(api_path, usage)

    command_path = "src-tauri/src/commands/usage.rs"
    commands = read(command_path)
    commands = replace_once(
        commands,
        "use crate::api::usage::{get_account_usage, refresh_all_usage, warmup_account as send_warmup};\n",
        "use crate::api::usage::get_account_reset_cards_terminal_output;\nuse crate::api::usage::{get_account_usage, refresh_all_usage, warmup_account as send_warmup};\n",
        "usage command imports",
    )
    command_block = r'''
/// Get terminal-style Codex reset-card output for one account.
#[tauri::command]
pub async fn get_reset_cards(account_id: String) -> Result<String, String> {
    let account = get_account(&account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    get_account_reset_cards_terminal_output(&account)
        .await
        .map_err(|e| e.to_string())
}

'''
    commands = replace_once(
        commands,
        "/// Refresh usage info for all accounts\n#[tauri::command]",
        command_block + "/// Refresh usage info for all accounts\n#[tauri::command]",
        "reset card command insertion point",
    )
    write(command_path, commands)

    lib_path = "src-tauri/src/lib.rs"
    lib = read(lib_path)
    lib = replace_once(
        lib,
        "get_masked_account_ids, get_usage, import_accounts_full_encrypted_file,",
        "get_masked_account_ids, get_reset_cards, get_usage, import_accounts_full_encrypted_file,",
        "lib command import",
    )
    lib = replace_once(
        lib,
        "            get_usage,\n            refresh_all_accounts_usage,",
        "            get_usage,\n            get_reset_cards,\n            refresh_all_accounts_usage,",
        "lib invoke handler",
    )
    write(lib_path, lib)

    verify_backend()


def patch_frontend() -> None:
    path = "src/components/AccountCard.tsx"
    card = read(path)
    card = replace_once(
        card,
        'import type { AccountWithUsage } from "../types";\n',
        'import type { AccountWithUsage } from "../types";\nimport { invokeBackend } from "../lib/platform";\n',
        "AccountCard platform import",
    )
    card = replace_once(
        card,
        "  const [isEditing, setIsEditing] = useState(false);\n  const [editName, setEditName] = useState(account.name);",
        "  const [isEditing, setIsEditing] = useState(false);\n  const [editName, setEditName] = useState(account.name);\n  const [isResetCardOpen, setIsResetCardOpen] = useState(false);\n  const [isFetchingResetCards, setIsFetchingResetCards] = useState(false);\n  const [resetCardOutput, setResetCardOutput] = useState(\"\");",
        "reset card state",
    )
    handler = r'''
  const handleResetCards = async () => {
    setIsResetCardOpen(true);
    setIsFetchingResetCards(true);
    setResetCardOutput("Fetching reset cards...");

    try {
      const output = await invokeBackend<string>("get_reset_cards", {
        accountId: account.id,
      });
      setResetCardOutput(output);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setResetCardOutput(`Error: ${message}`);
    } finally {
      setIsFetchingResetCards(false);
    }
  };

'''
    card = replace_once(
        card,
        "  const handleRename = async () => {\n",
        handler + "  const handleRename = async () => {\n",
        "reset card click handler",
    )
    button = r'''        <button
          onClick={() => {
            void handleResetCards();
          }}
          disabled={isFetchingResetCards || account.auth_mode !== "chat_g_p_t"}
          className={`px-3 py-2 text-xs font-medium rounded-lg transition-colors whitespace-nowrap ${
            isFetchingResetCards
              ? "bg-violet-100 dark:bg-violet-900/30 text-violet-500 dark:text-violet-300"
              : "bg-violet-50 dark:bg-violet-900/20 hover:bg-violet-100 dark:hover:bg-violet-900/40 text-violet-700 dark:text-violet-300"
          } disabled:opacity-50`}
          title={account.auth_mode === "chat_g_p_t" ? "Fetch reset cards manually" : "Reset cards are only available for ChatGPT accounts"}
        >
          {isFetchingResetCards ? "Cards..." : "Cards"}
        </button>
'''
    card = replace_once(
        card,
        "        <button\n          onClick={handleRefresh}",
        button + "        <button\n          onClick={handleRefresh}",
        "reset card button",
    )
    modal = r'''

      {isResetCardOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/45 px-4 py-6">
          <div className="flex max-h-[85vh] w-full max-w-3xl flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
            <div className="flex items-start justify-between gap-4 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
              <div className="min-w-0">
                <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
                  Reset cards
                </h2>
                <p className="mt-1 truncate text-sm text-gray-500 dark:text-gray-400">
                  {account.name}{account.email ? ` · ${account.email}` : ""}
                </p>
              </div>
              <button
                onClick={() => setIsResetCardOpen(false)}
                className="rounded-lg px-3 py-1.5 text-sm font-medium text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-100"
              >
                Close
              </button>
            </div>
            <div className="min-h-0 flex-1 overflow-auto bg-gray-950 p-4">
              <pre className="whitespace-pre-wrap break-words font-mono text-xs leading-5 text-gray-100">
                {resetCardOutput}
              </pre>
            </div>
          </div>
        </div>
      )}
'''
    card = replace_once(
        card,
        "      </div>\n    </div>\n  );\n}",
        "      </div>" + modal + "\n    </div>\n  );\n}",
        "reset card modal",
    )
    write(path, card)

    verify_frontend()


def verify_manual_usage() -> None:
    hook = read("src/hooks/useAccounts.ts")
    app = read("src/App.tsx")
    if "refreshUsage().catch(() => {})" in hook or "loadAccounts().then((accountList) => refreshUsage(accountList))" in hook:
        raise RuntimeError("automatic startup or interval usage refresh remains")
    if "await refreshUsage(accountList);" in hook or "await refreshUsage(accountList);" in app:
        raise RuntimeError("post-login/import usage refresh remains")
    if "const refreshSingleUsage" not in hook or "await refreshUsage()" not in app:
        raise RuntimeError("manual usage refresh path is missing")
    if "warmup_account" not in hook or "warmup_all_accounts" not in hook:
        raise RuntimeError("warmup path changed unexpectedly")


def verify_backend() -> None:
    usage = read("src-tauri/src/api/usage.rs")
    commands = read("src-tauri/src/commands/usage.rs")
    lib = read("src-tauri/src/lib.rs")
    required = [
        "CHATGPT_RESET_CARDS_API" in usage,
        "get_account_reset_cards_terminal_output" in usage,
        "Complete response body" in usage,
        "get_reset_cards" in commands,
        "get_reset_cards" in lib,
    ]
    if not all(required):
        raise RuntimeError("reset card backend command is incomplete")


def verify_frontend() -> None:
    card = read("src/components/AccountCard.tsx")
    required = [
        "get_reset_cards" in card,
        "isResetCardOpen" in card,
        "Reset cards" in card,
        "account.name" in card,
        "account.email" in card,
        "whitespace-pre-wrap" in card,
    ]
    if not all(required):
        raise RuntimeError("reset card frontend is incomplete")


def verify_all() -> None:
    verify_manual_usage()
    verify_backend()
    verify_frontend()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("stage", choices=["manual", "backend", "frontend", "verify"])
    args = parser.parse_args()

    if args.stage == "manual":
        patch_manual_usage()
    elif args.stage == "backend":
        patch_backend()
    elif args.stage == "frontend":
        patch_frontend()
    else:
        verify_all()


if __name__ == "__main__":
    main()
