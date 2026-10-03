//! Usage query Tauri commands

use crate::account_data_cache::{self, CachedAccountData, CachedDataset};
use crate::api::usage::{
    fetch_chatgpt_account_metadata, get_account_usage, refresh_all_usage,
    warmup_account as send_warmup,
};
use crate::auth::{ensure_chatgpt_tokens_fresh, get_account, load_accounts};
use crate::types::{AccountInfo, AuthData, UsageInfo, WarmupSummary};
use futures::{stream, StreamExt};

pub(crate) fn apply_cached_account_metadata(
    account: &mut AccountInfo,
    cached_data: &[CachedAccountData],
) {
    let Some(metadata) = cached_data
        .iter()
        .find(|cached| cached.account_id == account.id)
        .and_then(|cached| cached.metadata.as_ref())
    else {
        return;
    };

    if metadata.plan_type.is_some() {
        account.plan_type = metadata.plan_type.clone();
    }
    account.subscription_expires_at = metadata.subscription_expires_at;
}

/// Read the shared cache without making network requests.
#[tauri::command]
pub fn get_cached_account_data() -> Vec<CachedAccountData> {
    account_data_cache::get_cached_account_data()
}

/// Fetch usage info for a specific account (shared by the Tauri command and web mode).
pub async fn fetch_usage(account_id: &str) -> Result<UsageInfo, String> {
    let account = get_account(account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    let ticket = account_data_cache::begin_refresh(&account, CachedDataset::Usage);
    let usage = get_account_usage(&account)
        .await
        .map_err(|e| e.to_string())?;
    if usage.error.is_none() && !account_data_cache::record_usage(&ticket, &account, &usage)? {
        return Err(
            "The usage result was superseded or could not be assigned to this account".into(),
        );
    }
    Ok(usage)
}

/// Get usage info for a specific account
#[tauri::command]
pub async fn get_usage(account_id: String) -> Result<UsageInfo, String> {
    fetch_usage(&account_id).await
}

/// Refresh account metadata for a specific account.
/// For ChatGPT accounts this ensures OAuth tokens are valid and pulls live subscription metadata.
/// For API key accounts this is a no-op.
#[tauri::command]
pub async fn refresh_account_metadata(account_id: String) -> Result<AccountInfo, String> {
    let account = get_account(&account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    let (updated, live_metadata) = match &account.auth_data {
        AuthData::ApiKey { .. } => (account, None),
        AuthData::ChatGPT { .. } => {
            let ticket = account_data_cache::begin_refresh(&account, CachedDataset::Metadata);
            let refreshed = ensure_chatgpt_tokens_fresh(&account)
                .await
                .map_err(|e| e.to_string())?;
            let live_metadata = fetch_chatgpt_account_metadata(&refreshed)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(identity) = account_data_cache::stable_account_identity(&account) {
                if !account_data_cache::is_current_identity(&account_id, &identity) {
                    return Err("Account identity changed during metadata refresh".to_string());
                }
            }
            if !account_data_cache::record_metadata(&ticket, &account, &live_metadata)? {
                return Err(
                    "The metadata result was superseded or could not be assigned to this account"
                        .to_string(),
                );
            }

            (refreshed, Some(live_metadata))
        }
    };

    let store = load_accounts().map_err(|e| e.to_string())?;
    let active_id = store.active_account_id.as_deref();
    let mut info = AccountInfo::from_stored(&updated, active_id);
    if let Some(metadata) = live_metadata {
        if metadata.plan_type.is_some() {
            info.plan_type = metadata.plan_type;
        }
        info.subscription_expires_at = metadata.subscription_expires_at;
    }
    Ok(info)
}

/// Refresh usage info for all accounts
#[tauri::command]
pub async fn refresh_all_accounts_usage() -> Result<Vec<UsageInfo>, String> {
    let store = load_accounts().map_err(|e| e.to_string())?;
    let tickets: std::collections::HashMap<_, _> = store
        .accounts
        .iter()
        .map(|account| {
            (
                account.id.clone(),
                account_data_cache::begin_refresh(account, CachedDataset::Usage),
            )
        })
        .collect();
    let results = refresh_all_usage(&store.accounts).await;
    for usage in &results {
        if let (Some(ticket), Some(account)) = (
            tickets.get(&usage.account_id),
            store
                .accounts
                .iter()
                .find(|account| account.id == usage.account_id),
        ) {
            if usage.error.is_none() && !account_data_cache::record_usage(ticket, account, usage)? {
                return Err(
                    "An account usage result was superseded or could not be assigned to its account"
                        .to_string(),
                );
            }
        }
    }
    Ok(results)
}

/// Send a minimal warm-up request for one account
#[tauri::command]
pub async fn warmup_account(account_id: String) -> Result<(), String> {
    let account = get_account(&account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Account not found: {account_id}"))?;

    send_warmup(&account).await.map_err(|e| e.to_string())?;
    account_data_cache::invalidate_after_warmup_result(&account)
        .map_err(|error| format!("Warm-up completed but cache invalidation failed: {error}"))?;
    Ok(())
}

/// Send minimal warm-up requests for all accounts
#[tauri::command]
pub async fn warmup_all_accounts() -> Result<WarmupSummary, String> {
    let store = load_accounts().map_err(|e| e.to_string())?;
    let total_accounts = store.accounts.len();
    let concurrency = total_accounts.min(10).max(1);

    let results: Vec<(String, bool)> = stream::iter(store.accounts.into_iter())
        .map(|account| async move {
            let account_id = account.id.clone();
            let failed = send_warmup(&account).await.is_err();
            if !failed {
                if account_data_cache::invalidate_after_warmup_result(&account).is_err() {
                    return (account_id, true);
                }
            }
            (account_id, failed)
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let failed_account_ids = results
        .into_iter()
        .filter_map(|(account_id, failed)| failed.then_some(account_id))
        .collect::<Vec<_>>();

    let warmed_accounts = total_accounts.saturating_sub(failed_account_ids.len());
    Ok(WarmupSummary {
        total_accounts,
        warmed_accounts,
        failed_account_ids,
    })
}
