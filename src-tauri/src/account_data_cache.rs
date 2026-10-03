//! Persistent, local-only cache for account usage, statistics, and metadata.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, OnceLock},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use uuid::Uuid;

use crate::{
    api::usage::ChatGptAccountMetadata,
    auth::{get_config_dir, load_accounts},
    commands::account_stats::AccountUsageStats,
    types::{parse_chatgpt_id_token_claims, AuthData, StoredAccount, UsageInfo},
};

const CACHE_VERSION: u32 = 1;
const CACHE_FILE_NAME: &str = "account-data-v1.json";
const CACHE_TTL_MS: i64 = 5 * 60 * 1000;
const CACHE_CHANGED_EVENT: &str = "usage-cache-changed";

static CACHE_FILE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static REQUEST_VERSIONS: LazyLock<Mutex<HashMap<String, RequestVersions>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MASKED_DATASETS: LazyLock<Mutex<HashSet<(String, String, CachedDataset)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[cfg(desktop)]
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
pub struct CachedAccountData {
    pub account_id: String,
    pub usage: Option<UsageInfo>,
    pub usage_fetched_at: Option<i64>,
    pub stats: Option<AccountUsageStats>,
    pub stats_fetched_at: Option<i64>,
    pub metadata: Option<ChatGptAccountMetadata>,
    pub metadata_fetched_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccountDataCacheFile {
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, AccountDataCacheEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AccountDataCacheEntry {
    identity: String,
    usage: Option<UsageInfo>,
    usage_fetched_at: Option<i64>,
    stats: Option<AccountUsageStats>,
    stats_fetched_at: Option<i64>,
    metadata: Option<ChatGptAccountMetadata>,
    metadata_fetched_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CachedDataset {
    Usage,
    Stats,
    Metadata,
}

#[derive(Debug, Clone)]
pub struct RefreshTicket {
    account_id: String,
    identity: Option<String>,
    dataset: CachedDataset,
    sequence: u64,
}

#[derive(Debug, Default)]
struct RequestVersions {
    usage: u64,
    stats: u64,
    metadata: u64,
}

impl RequestVersions {
    fn get(&self, dataset: CachedDataset) -> u64 {
        match dataset {
            CachedDataset::Usage => self.usage,
            CachedDataset::Stats => self.stats,
            CachedDataset::Metadata => self.metadata,
        }
    }

    fn bump(&mut self, dataset: CachedDataset) -> u64 {
        let sequence = match dataset {
            CachedDataset::Usage => &mut self.usage,
            CachedDataset::Stats => &mut self.stats,
            CachedDataset::Metadata => &mut self.metadata,
        };
        *sequence = sequence.wrapping_add(1);
        *sequence
    }

    fn bump_usage_and_stats(&mut self) {
        self.bump(CachedDataset::Usage);
        self.bump(CachedDataset::Stats);
    }

    fn bump_all(&mut self) {
        self.bump(CachedDataset::Usage);
        self.bump(CachedDataset::Stats);
        self.bump(CachedDataset::Metadata);
    }
}

/// Stable identity used to keep a local account UUID from inheriting another account's data.
/// OAuth token contents are deliberately excluded so token rotation keeps the same identity.
pub fn stable_account_identity(account: &StoredAccount) -> Option<String> {
    match &account.auth_data {
        AuthData::ChatGPT {
            id_token,
            account_id,
            ..
        } => parse_chatgpt_id_token_claims(id_token)
            .account_id
            .or_else(|| account_id.clone())
            .map(|id| format!("chatgpt:{id}"))
            .or_else(|| {
                account
                    .email
                    .as_ref()
                    .filter(|email| !email.trim().is_empty())
                    .map(|email| format!("chatgpt-email:{}", email.trim().to_ascii_lowercase()))
            }),
        // API-key usage and metadata endpoints do not produce cacheable account data.
        AuthData::ApiKey { .. } => Some("api-key".to_string()),
    }
}

pub fn begin_refresh(account: &StoredAccount, dataset: CachedDataset) -> RefreshTicket {
    let mut versions = REQUEST_VERSIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let sequence = versions
        .entry(account.id.clone())
        .or_default()
        .bump(dataset);

    RefreshTicket {
        account_id: account.id.clone(),
        identity: stable_account_identity(account),
        dataset,
        sequence,
    }
}

pub fn is_current_identity(account_id: &str, expected_identity: &str) -> bool {
    load_accounts()
        .ok()
        .and_then(|store| {
            store
                .accounts
                .into_iter()
                .find(|account| account.id == account_id)
        })
        .and_then(|account| stable_account_identity(&account))
        .as_deref()
        == Some(expected_identity)
}

pub fn record_usage(
    ticket: &RefreshTicket,
    account: &StoredAccount,
    usage: &UsageInfo,
) -> Result<bool, String> {
    if usage.error.is_some() || usage.account_id != account.id {
        return Ok(false);
    }

    let saved = update_after_refresh(ticket, account, |entry, fetched_at| {
        entry.usage = Some(usage.clone());
        entry.usage_fetched_at = Some(fetched_at);
    })?;
    if saved {
        clear_dataset_mask(
            &account.id,
            ticket.identity.as_deref(),
            CachedDataset::Usage,
        );
    }
    Ok(saved)
}

pub fn record_stats(
    ticket: &RefreshTicket,
    account: &StoredAccount,
    stats: &AccountUsageStats,
) -> Result<bool, String> {
    if !stats.available || stats.error.is_some() || stats.account_id != account.id {
        return Ok(false);
    }

    let saved = update_after_refresh(ticket, account, |entry, fetched_at| {
        entry.stats = Some(stats.clone());
        entry.stats_fetched_at = Some(fetched_at);
    })?;
    if saved {
        clear_dataset_mask(
            &account.id,
            ticket.identity.as_deref(),
            CachedDataset::Stats,
        );
    }
    Ok(saved)
}

pub fn record_metadata(
    ticket: &RefreshTicket,
    account: &StoredAccount,
    metadata: &ChatGptAccountMetadata,
) -> Result<bool, String> {
    let saved = update_after_refresh(ticket, account, |entry, fetched_at| {
        entry.metadata = Some(metadata.clone());
        entry.metadata_fetched_at = Some(fetched_at);
    })?;
    if saved {
        clear_dataset_mask(
            &account.id,
            ticket.identity.as_deref(),
            CachedDataset::Metadata,
        );
    }
    Ok(saved)
}

fn clear_dataset_mask(account_id: &str, identity: Option<&str>, dataset: CachedDataset) {
    let Some(identity) = identity else {
        return;
    };
    if let Ok(mut invalidated) = MASKED_DATASETS.lock() {
        invalidated.remove(&(account_id.to_string(), identity.to_string(), dataset));
    }
}

fn update_after_refresh(
    ticket: &RefreshTicket,
    account: &StoredAccount,
    update: impl FnOnce(&mut AccountDataCacheEntry, i64),
) -> Result<bool, String> {
    if ticket.account_id != account.id {
        return Ok(false);
    }
    let Some(identity) = ticket.identity.as_deref() else {
        return Ok(false);
    };
    if stable_account_identity(account).as_deref() != Some(identity) {
        return Ok(false);
    }

    let _file_lock = CACHE_FILE_LOCK
        .lock()
        .map_err(|_| "Account data cache is unavailable".to_string())?;
    let mut versions = REQUEST_VERSIONS
        .lock()
        .map_err(|_| "Account refresh sequence is unavailable".to_string())?;
    if versions
        .get(&ticket.account_id)
        .map(|current| current.get(ticket.dataset))
        != Some(ticket.sequence)
    {
        return Ok(false);
    }

    let current_account = match load_accounts() {
        Ok(store) => store
            .accounts
            .into_iter()
            .find(|current| current.id == ticket.account_id),
        Err(error) => {
            return Err(format!(
                "Failed to validate account before caching data: {error}"
            ));
        }
    };
    if current_account
        .as_ref()
        .and_then(stable_account_identity)
        .as_deref()
        != Some(identity)
    {
        versions
            .entry(ticket.account_id.clone())
            .or_default()
            .bump_all();
        let path = cache_file_path().map_err(|error| error.to_string())?;
        let mut cache = read_cache_file(&path).unwrap_or_default();
        let removed = cache.accounts.remove(&ticket.account_id).is_some();
        if removed {
            write_cache_file(&path, &cache).map_err(|error| error.to_string())?;
        }
        drop(versions);
        drop(_file_lock);
        if removed {
            notify_cache_changed();
        }
        return Ok(false);
    }

    let path = cache_file_path().map_err(|error| error.to_string())?;
    let mut cache = read_cache_file(&path).unwrap_or_default();
    cache.version = CACHE_VERSION;
    let entry = cache.accounts.entry(ticket.account_id.clone()).or_default();
    if entry.identity != identity {
        *entry = AccountDataCacheEntry::default();
        entry.identity = identity.to_string();
    }
    entry.identity = identity.to_string();
    update(entry, Utc::now().timestamp_millis());

    write_cache_file(&path, &cache).map_err(|error| error.to_string())?;
    drop(versions);
    drop(_file_lock);
    notify_cache_changed();
    Ok(true)
}

/// A successful warm-up can change quota and profile state, so hide those snapshots.
pub fn invalidate_after_warmup_result(account: &StoredAccount) -> Result<(), String> {
    let _file_lock = CACHE_FILE_LOCK
        .lock()
        .map_err(|_| "Account data cache is unavailable".to_string())?;
    let mut versions = REQUEST_VERSIONS
        .lock()
        .map_err(|_| "Account refresh sequence is unavailable".to_string())?;
    let account_versions = versions.entry(account.id.clone()).or_default();
    account_versions.bump_usage_and_stats();

    let current_identity = load_accounts()
        .ok()
        .and_then(|store| {
            store
                .accounts
                .into_iter()
                .find(|current| current.id == account.id)
        })
        .and_then(|current| stable_account_identity(&current));
    if current_identity.as_deref() != stable_account_identity(account).as_deref() {
        account_versions.bump_all();
    }

    let Some(identity) = stable_account_identity(account) else {
        return Ok(());
    };
    {
        let mut invalidated = MASKED_DATASETS
            .lock()
            .map_err(|_| "Account cache invalidation state is unavailable".to_string())?;
        invalidated.insert((account.id.clone(), identity.clone(), CachedDataset::Usage));
        invalidated.insert((account.id.clone(), identity.clone(), CachedDataset::Stats));
    }

    let path = cache_file_path().map_err(|error| error.to_string())?;
    let mut cache = read_cache_file(&path).map_err(|error| error.to_string())?;
    let Some(entry) = cache.accounts.get_mut(&account.id) else {
        drop(versions);
        drop(_file_lock);
        notify_cache_changed();
        return Ok(());
    };
    if current_identity.as_deref() != Some(entry.identity.as_str()) || entry.identity != identity {
        cache.accounts.remove(&account.id);
        write_cache_file(&path, &cache).map_err(|error| error.to_string())?;
        drop(versions);
        drop(_file_lock);
        notify_cache_changed();
        return Ok(());
    }

    entry.usage = None;
    entry.stats = None;
    let write_result = write_cache_file(&path, &cache).map_err(|error| error.to_string());
    drop(versions);
    drop(_file_lock);
    notify_cache_changed();
    write_result
}

/// Remove a deleted account's cache and invalidate all refreshes that were already running.
pub fn remove_account_cache(account_id: &str) -> Result<(), String> {
    let Ok(_file_lock) = CACHE_FILE_LOCK.lock() else {
        return Err("Account data cache is unavailable".to_string());
    };
    let Ok(mut versions) = REQUEST_VERSIONS.lock() else {
        return Err("Account refresh sequence is unavailable".to_string());
    };
    versions
        .entry(account_id.to_string())
        .or_default()
        .bump_all();

    let path = cache_file_path().map_err(|error| error.to_string())?;
    let mut cache = read_cache_file(&path).unwrap_or_default();
    let write_result = if cache.accounts.remove(account_id).is_some() {
        write_cache_file(&path, &cache).map_err(|error| error.to_string())
    } else {
        Ok(())
    };
    if let Ok(mut invalidated) = MASKED_DATASETS.lock() {
        invalidated.retain(|(cached_id, _, _)| cached_id != account_id);
    }
    drop(versions);
    drop(_file_lock);
    notify_cache_changed();
    write_result
}

/// Load the shared cache from disk without making any network requests.
pub fn get_cached_account_data() -> Vec<CachedAccountData> {
    let accounts = match load_accounts() {
        Ok(store) => store.accounts,
        Err(error) => {
            eprintln!("Failed to load accounts for cached data: {error}");
            return Vec::new();
        }
    };
    get_cached_account_data_for_accounts(&accounts)
}

pub fn get_cached_account_data_for_accounts(accounts: &[StoredAccount]) -> Vec<CachedAccountData> {
    let Ok(_file_lock) = CACHE_FILE_LOCK.lock() else {
        return empty_cached_data(accounts);
    };
    let path = match cache_file_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Failed to resolve account data cache path: {error:#}");
            return empty_cached_data(accounts);
        }
    };
    let mut cache = match read_cache_file(&path) {
        Ok(cache) => cache,
        Err(error) => {
            eprintln!("Failed to read account data cache: {error:#}");
            AccountDataCacheFile::default()
        }
    };

    let identities: HashMap<&str, String> = accounts
        .iter()
        .filter_map(|account| {
            stable_account_identity(account).map(|identity| (account.id.as_str(), identity))
        })
        .collect();
    let now = Utc::now().timestamp_millis();
    let invalidated_cache_fields = invalidate_expired_cache_fields(&mut cache, now);
    if let Ok(mut masked) = MASKED_DATASETS.lock() {
        for key in &invalidated_cache_fields {
            masked.insert(key.clone());
        }
    }

    let original_len = cache.accounts.len();
    cache.accounts.retain(|account_id, entry| {
        identities
            .get(account_id.as_str())
            .is_some_and(|identity| identity == &entry.identity)
    });
    let stale_entries_removed = cache.accounts.len() != original_len;
    let cache_changed = stale_entries_removed || !invalidated_cache_fields.is_empty();
    let cleanup_succeeded = !cache_changed || write_cache_file(&path, &cache).is_ok();
    drop(_file_lock);
    if cache_changed && cleanup_succeeded {
        notify_cache_changed();
    }

    let invalidated = MASKED_DATASETS.lock().ok();
    accounts
        .iter()
        .map(|account| {
            let mut data = cached_data_for(account, cache.accounts.get(&account.id), now);
            if let (Some(identity), Some(invalidated)) =
                (stable_account_identity(account), invalidated.as_ref())
            {
                if invalidated.contains(&(
                    account.id.clone(),
                    identity.clone(),
                    CachedDataset::Usage,
                )) {
                    data.usage = None;
                }
                if invalidated.contains(&(account.id.clone(), identity, CachedDataset::Stats)) {
                    data.stats = None;
                }
            }
            data
        })
        .collect()
}

fn empty_cached_data(accounts: &[StoredAccount]) -> Vec<CachedAccountData> {
    accounts
        .iter()
        .map(|account| CachedAccountData {
            account_id: account.id.clone(),
            usage: None,
            usage_fetched_at: None,
            stats: None,
            stats_fetched_at: None,
            metadata: None,
            metadata_fetched_at: None,
        })
        .collect()
}

fn invalidate_expired_cache_fields(
    cache: &mut AccountDataCacheFile,
    now_ms: i64,
) -> Vec<(String, String, CachedDataset)> {
    let mut invalidated = Vec::new();
    for (account_id, entry) in &mut cache.accounts {
        let identity = entry.identity.clone();
        if entry.usage.is_some()
            && !entry
                .usage_fetched_at
                .is_some_and(|fetched_at| cache_timestamp_is_fresh(fetched_at, now_ms))
        {
            entry.usage = None;
            invalidated.push((account_id.clone(), identity.clone(), CachedDataset::Usage));
        }
        if entry.stats.is_some()
            && !entry
                .stats_fetched_at
                .is_some_and(|fetched_at| cache_timestamp_is_fresh(fetched_at, now_ms))
        {
            entry.stats = None;
            invalidated.push((account_id.clone(), identity.clone(), CachedDataset::Stats));
        }
        let metadata_expired_after_fetch =
            match (entry.metadata.as_ref(), entry.metadata_fetched_at) {
                (Some(metadata), Some(fetched_at)) => {
                    subscription_expiry_passed_after_fetch(metadata, fetched_at, now_ms)
                }
                _ => false,
            };
        if entry.metadata.is_some()
            && (!entry
                .metadata_fetched_at
                .is_some_and(|fetched_at| cache_timestamp_is_fresh(fetched_at, now_ms))
                || metadata_expired_after_fetch)
        {
            if entry.metadata.take().is_some() {
                invalidated.push((account_id.clone(), identity, CachedDataset::Metadata));
            }
        }
    }
    invalidated
}

fn cached_data_for(
    account: &StoredAccount,
    entry: Option<&AccountDataCacheEntry>,
    now_ms: i64,
) -> CachedAccountData {
    let Some(entry) = entry.filter(|entry| {
        stable_account_identity(account).as_deref() == Some(entry.identity.as_str())
    }) else {
        return empty_cached_data(std::slice::from_ref(account))
            .into_iter()
            .next()
            .expect("one empty entry is returned for the given account");
    };

    let usage_fresh = entry
        .usage_fetched_at
        .is_some_and(|fetched_at| cache_timestamp_is_fresh(fetched_at, now_ms));
    let stats_fresh = entry
        .stats_fetched_at
        .is_some_and(|fetched_at| cache_timestamp_is_fresh(fetched_at, now_ms));
    let metadata_fresh = entry.metadata_fetched_at.is_some_and(|fetched_at| {
        cache_timestamp_is_fresh(fetched_at, now_ms)
            && entry.metadata.as_ref().is_some_and(|metadata| {
                !subscription_expiry_passed_after_fetch(metadata, fetched_at, now_ms)
            })
    });

    CachedAccountData {
        account_id: account.id.clone(),
        usage: usage_fresh
            .then(|| {
                entry
                    .usage
                    .as_ref()
                    .map(|usage| hide_expired_usage_windows(usage, now_ms))
            })
            .flatten(),
        usage_fetched_at: entry.usage_fetched_at,
        stats: stats_fresh.then(|| entry.stats.clone()).flatten(),
        stats_fetched_at: entry.stats_fetched_at,
        metadata: metadata_fresh.then(|| entry.metadata.clone()).flatten(),
        metadata_fetched_at: entry.metadata_fetched_at,
    }
}

fn cache_timestamp_is_fresh(fetched_at_ms: i64, now_ms: i64) -> bool {
    now_ms
        .checked_sub(fetched_at_ms)
        .is_some_and(|age_ms| age_ms >= 0 && age_ms < CACHE_TTL_MS)
}

fn hide_expired_usage_windows(usage: &UsageInfo, now_ms: i64) -> UsageInfo {
    let mut visible = usage.clone();
    let primary_expired = usage
        .primary_resets_at
        .and_then(|timestamp| i64::from(timestamp).checked_mul(1000))
        .is_some_and(|reset_at_ms| reset_at_ms <= now_ms);
    if primary_expired {
        visible.primary_used_percent = None;
        visible.primary_window_minutes = None;
        visible.primary_resets_at = None;
    }

    let secondary_expired = usage
        .secondary_resets_at
        .and_then(|timestamp| i64::from(timestamp).checked_mul(1000))
        .is_some_and(|reset_at_ms| reset_at_ms <= now_ms);
    if secondary_expired {
        visible.secondary_used_percent = None;
        visible.secondary_window_minutes = None;
        visible.secondary_resets_at = None;
    }

    visible
}

fn subscription_expiry_passed_after_fetch(
    metadata: &ChatGptAccountMetadata,
    fetched_at_ms: i64,
    now_ms: i64,
) -> bool {
    metadata
        .subscription_expires_at
        .as_ref()
        .map(|expires_at| expires_at.timestamp_millis())
        .is_some_and(|expires_at_ms| expires_at_ms > fetched_at_ms && expires_at_ms <= now_ms)
}

fn cache_file_path() -> Result<PathBuf> {
    Ok(get_config_dir()?.join(CACHE_FILE_NAME))
}

fn read_cache_file(path: &Path) -> Result<AccountDataCacheFile> {
    if !path.exists() {
        return Ok(AccountDataCacheFile {
            version: CACHE_VERSION,
            accounts: HashMap::new(),
        });
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read cache file: {}", path.display()))?;
    let cache: AccountDataCacheFile = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse cache file: {}", path.display()))?;
    if cache.version != CACHE_VERSION {
        return Ok(AccountDataCacheFile {
            version: CACHE_VERSION,
            accounts: HashMap::new(),
        });
    }
    Ok(cache)
}

fn write_cache_file(path: &Path, cache: &AccountDataCacheFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create cache directory: {}", parent.display()))?;
    }

    let content = serde_json::to_vec_pretty(cache).context("Failed to serialize account cache")?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(CACHE_FILE_NAME);
    let temporary = path.with_file_name(format!("{filename}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("Failed to create cache temp file: {}", temporary.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(&content)
            .with_context(|| format!("Failed to write cache temp file: {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync cache temp file: {}", temporary.display()))?;
        set_restrictive_permissions(&temporary)?;
        fs::rename(&temporary, path).with_context(|| {
            format!("Failed to atomically replace cache file {}", path.display())
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn set_restrictive_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "Failed to restrict cache file permissions: {}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn set_restrictive_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(desktop)]
pub fn register_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

fn notify_cache_changed() {
    #[cfg(desktop)]
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit(CACHE_CHANGED_EVENT, ());
        crate::tray::refresh(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::account_stats::{
        AccountDailyUsage, AccountResetCredit, AccountResetCredits, AccountUsageActivity,
        AccountUsageSummary,
    };

    fn account(identity: &str, local_id: &str) -> StoredAccount {
        StoredAccount {
            id: local_id.to_string(),
            name: "Test account".to_string(),
            email: Some("test@example.com".to_string()),
            plan_type: None,
            subscription_expires_at: None,
            auth_mode: crate::types::AuthMode::ChatGPT,
            auth_data: AuthData::ChatGPT {
                id_token: "opaque".to_string(),
                access_token: "test-access-token".to_string(),
                refresh_token: "test-refresh-token".to_string(),
                account_id: Some(identity.to_string()),
            },
            created_at: Utc::now(),
            last_used_at: None,
        }
    }

    fn usage(account_id: &str, reset_at: Option<i64>) -> UsageInfo {
        UsageInfo {
            account_id: account_id.to_string(),
            plan_type: Some("plus".to_string()),
            primary_used_percent: Some(40.0),
            primary_window_minutes: Some(300),
            primary_resets_at: reset_at,
            secondary_used_percent: Some(20.0),
            secondary_window_minutes: Some(10_080),
            secondary_resets_at: None,
            has_credits: None,
            unlimited_credits: None,
            credits_balance: None,
            error: None,
        }
    }

    fn stats(account_id: &str, expiry: Option<String>) -> AccountUsageStats {
        AccountUsageStats {
            account_id: account_id.to_string(),
            available: true,
            source: "test".to_string(),
            generated_at: None,
            stats_as_of: None,
            summary: AccountUsageSummary::default(),
            activity: AccountUsageActivity::default(),
            daily: vec![AccountDailyUsage {
                date: "2026-10-03".to_string(),
                tokens: 10,
            }],
            top_invocations: Vec::new(),
            reset_credits: expiry.map(|expires_at| AccountResetCredits {
                available_count: 1,
                next_expires_at: Some(expires_at.clone()),
                credits: vec![AccountResetCredit {
                    id: "credit-1".to_string(),
                    reset_type: "test".to_string(),
                    status: "available".to_string(),
                    granted_at: None,
                    expires_at: Some(expires_at),
                    redeem_started_at: None,
                    redeemed_at: None,
                    title: None,
                    description: None,
                }],
            }),
            error: None,
        }
    }

    fn cache_entry(account: &StoredAccount, fetched_at: i64) -> AccountDataCacheEntry {
        AccountDataCacheEntry {
            identity: stable_account_identity(account).unwrap(),
            usage: Some(usage(&account.id, None)),
            usage_fetched_at: Some(fetched_at),
            stats: Some(stats(&account.id, None)),
            stats_fetched_at: Some(fetched_at),
            metadata: Some(ChatGptAccountMetadata {
                plan_type: Some("plus".to_string()),
                subscription_expires_at: None,
            }),
            metadata_fetched_at: Some(fetched_at),
        }
    }

    #[test]
    fn cache_survives_a_file_reopen_without_resetting_its_timestamp() {
        let account = account("workspace-a", "local-a");
        let fetched_at = 1_800_000_000_000;
        let mut cache = AccountDataCacheFile {
            version: CACHE_VERSION,
            accounts: HashMap::new(),
        };
        cache
            .accounts
            .insert(account.id.clone(), cache_entry(&account, fetched_at));

        let path = test_cache_path("restart");
        write_cache_file(&path, &cache).unwrap();
        let reopened = read_cache_file(&path).unwrap();
        let visible = cached_data_for(&account, reopened.accounts.get(&account.id), fetched_at + 1);

        assert!(visible.usage.is_some());
        assert_eq!(visible.usage_fetched_at, Some(fetched_at));
        assert_eq!(visible.stats_fetched_at, Some(fetched_at));
        assert_eq!(visible.metadata_fetched_at, Some(fetched_at));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ttl_expiry_and_clock_rollback_hide_data_but_keep_success_times() {
        let account = account("workspace-a", "local-a");
        let fetched_at = 1_800_000_000_000;
        let entry = cache_entry(&account, fetched_at);

        let expired = cached_data_for(&account, Some(&entry), fetched_at + CACHE_TTL_MS);
        assert!(expired.usage.is_none());
        assert_eq!(expired.usage_fetched_at, Some(fetched_at));
        assert!(expired.stats.is_none());
        assert!(expired.metadata.is_none());

        let future = cached_data_for(&account, Some(&entry), fetched_at - 1);
        assert!(future.usage.is_none());
        assert_eq!(future.usage_fetched_at, Some(fetched_at));
    }

    #[test]
    fn quota_reset_hides_only_the_expired_window_even_when_ttl_is_still_valid() {
        let account = account("workspace-a", "local-a");
        let fetched_at = 1_800_000_000_000;
        let mut entry = cache_entry(&account, fetched_at);
        entry.usage = Some(usage(&account.id, Some((fetched_at / 1000) + 60)));

        let visible = cached_data_for(&account, Some(&entry), fetched_at + 60_000);
        let visible_usage = visible.usage.unwrap();
        assert_eq!(visible_usage.primary_used_percent, None);
        assert_eq!(visible_usage.primary_window_minutes, None);
        assert_eq!(visible_usage.secondary_used_percent, Some(20.0));
        assert_eq!(visible.usage_fetched_at, Some(fetched_at));
    }

    #[test]
    fn reset_credit_expiry_does_not_expire_the_statistics_snapshot() {
        let account = account("workspace-a", "local-a");
        let fetched_at = 1_800_000_000_000;
        let mut entry = cache_entry(&account, fetched_at);
        entry.stats = Some(stats(
            &account.id,
            Some(
                DateTime::from_timestamp_millis(fetched_at - 30_000)
                    .unwrap()
                    .to_rfc3339(),
            ),
        ));

        let visible = cached_data_for(&account, Some(&entry), fetched_at + 30_000);
        assert!(visible.stats.is_some());
        assert_eq!(visible.stats_fetched_at, Some(fetched_at));
    }

    #[test]
    fn subscription_metadata_remains_visible_if_already_expired_when_fetched() {
        let account = account("workspace-a", "local-a");
        let fetched_at = 1_800_000_000_000;
        let mut entry = cache_entry(&account, fetched_at);
        entry.metadata.as_mut().unwrap().subscription_expires_at =
            Some(DateTime::from_timestamp_millis(fetched_at - 1).unwrap());

        let visible = cached_data_for(&account, Some(&entry), fetched_at + 1);

        assert!(visible.metadata.is_some());
    }

    #[test]
    fn subscription_metadata_hides_when_it_expires_after_fetch() {
        let account = account("workspace-a", "local-a");
        let fetched_at = 1_800_000_000_000;
        let mut entry = cache_entry(&account, fetched_at);
        entry.metadata.as_mut().unwrap().subscription_expires_at =
            Some(DateTime::from_timestamp_millis(fetched_at + 1_000).unwrap());

        let visible = cached_data_for(&account, Some(&entry), fetched_at + 1_000);

        assert!(visible.metadata.is_none());
    }

    #[test]
    fn identity_change_and_deletion_cannot_reuse_entries_or_late_tickets() {
        let original = account("workspace-a", "local-a");
        let replacement = account("workspace-b", "local-a");
        let entry = cache_entry(&original, 1_800_000_000_000);
        assert!(
            cached_data_for(&replacement, Some(&entry), 1_800_000_000_001)
                .usage
                .is_none()
        );

        let mut versions = RequestVersions::default();
        let old_sequence = versions.bump(CachedDataset::Usage);
        versions.bump_all(); // account deletion invalidates an in-flight completion
        assert_ne!(versions.get(CachedDataset::Usage), old_sequence);

        let mut file = AccountDataCacheFile {
            version: CACHE_VERSION,
            accounts: HashMap::from([(original.id.clone(), entry)]),
        };
        let known_ids = HashMap::<&str, String>::new();
        file.accounts
            .retain(|account_id, _| known_ids.contains_key(account_id.as_str()));
        assert!(file.accounts.is_empty());
    }

    fn test_cache_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-switcher-cache-{name}-{}.json",
            Uuid::new_v4()
        ))
    }
}
