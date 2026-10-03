//! In-process clock-time warm-up scheduling.
//!
//! The scheduler runs only while the desktop or LAN server process is alive.
//! It stores settings and the per-day fire ledger beside the account store and
//! never asks the operating system to start or wake the application.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Duration,
};

use chrono::{Local, NaiveDateTime};
use serde::{Deserialize, Serialize};

const SCHEDULE_FILE_NAME: &str = "warmup-schedule.json";
const COMPLETED_EVENT: &str = "timed-warmup-completed";
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const MAX_TICK_GAP_MS: i64 = 90_000;
const FULL_WEEKLY_QUOTA_THRESHOLD: f64 = 99.5;

static SCHEDULE_STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static SCHEDULER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WarmupSchedule {
    pub enabled: bool,
    pub times: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ScheduleStore {
    enabled: bool,
    times: Vec<String>,
    /// Local date (YYYY-MM-DD) of the last claimed run for each HH:MM time.
    last_fired: HashMap<String, String>,
}

impl ScheduleStore {
    fn settings(&self) -> WarmupSchedule {
        WarmupSchedule {
            enabled: self.enabled,
            times: normalize_times(&self.times),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
struct TimedWarmupCompletedPayload {
    warmed: usize,
    failed: usize,
}

type CompletionEmitter = Arc<dyn Fn(TimedWarmupCompletedPayload) + Send + Sync>;

/// Read saved schedule settings. A new installation is disabled by default.
#[tauri::command]
pub fn get_warmup_schedule() -> Result<WarmupSchedule, String> {
    let _guard = SCHEDULE_STORE_LOCK
        .lock()
        .map_err(|_| "Warm-up schedule storage is unavailable".to_string())?;
    Ok(load_store()?.settings())
}

/// Save schedule settings without changing the persisted per-day fire ledger.
#[tauri::command]
pub fn set_warmup_schedule(enabled: bool, times: Vec<String>) -> Result<WarmupSchedule, String> {
    let _guard = SCHEDULE_STORE_LOCK
        .lock()
        .map_err(|_| "Warm-up schedule storage is unavailable".to_string())?;
    let mut store = load_store()?;
    store.enabled = enabled;
    store.times = normalize_times(&times);
    save_store(&store)?;
    Ok(store.settings())
}

/// Start one scheduler for the desktop application process.
#[cfg(desktop)]
pub fn start(app: &tauri::AppHandle) {
    use tauri::Emitter;

    let app = app.clone();
    let emitter: CompletionEmitter = Arc::new(move |payload| {
        let _ = app.emit(COMPLETED_EVENT, payload);
    });
    start_inner(Some(emitter));
}

/// Start one scheduler for the standalone LAN server process.
pub fn start_for_web() {
    start_inner(None);
}

fn start_inner(emitter: Option<CompletionEmitter>) {
    if SCHEDULER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(CHECK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut previous_tick = Local::now().naive_local();

        loop {
            ticker.tick().await;
            let now = Local::now().naive_local();
            let due = match claim_due_times(previous_tick, now) {
                Ok(times) => times,
                Err(error) => {
                    eprintln!("[warmup schedule] Could not check schedule: {error}");
                    Vec::new()
                }
            };
            previous_tick = now;

            if !due.is_empty() {
                run_scheduled_warmup(emitter.clone()).await;
            }
        }
    });
}

async fn run_scheduled_warmup(emitter: Option<CompletionEmitter>) {
    let accounts = match crate::commands::list_accounts().await {
        Ok(accounts) => accounts,
        Err(error) => {
            eprintln!("[warmup schedule] Could not list accounts: {error}");
            emit_completion(
                emitter,
                TimedWarmupCompletedPayload {
                    warmed: 0,
                    failed: 1,
                },
            );
            return;
        }
    };

    let full_weekly_quota_ids: HashSet<String> =
        crate::account_data_cache::get_cached_account_data()
            .into_iter()
            .filter_map(|cached| {
                should_skip_for_full_weekly_quota(cached.usage.as_ref())
                    .then_some(cached.account_id)
            })
            .collect();

    let mut warmed = 0;
    let mut failed = 0;
    for account in accounts {
        if full_weekly_quota_ids.contains(&account.id) {
            continue;
        }

        match crate::commands::warmup_account(account.id).await {
            Ok(()) => warmed += 1,
            Err(error) => {
                failed += 1;
                eprintln!("[warmup schedule] Warm-up failed: {error}");
            }
        }
    }

    if warmed > 0 || failed > 0 {
        emit_completion(emitter, TimedWarmupCompletedPayload { warmed, failed });
    }
}

fn emit_completion(emitter: Option<CompletionEmitter>, payload: TimedWarmupCompletedPayload) {
    if let Some(emitter) = emitter {
        emitter(payload);
    }
}

fn should_skip_for_full_weekly_quota(usage: Option<&crate::types::UsageInfo>) -> bool {
    usage
        .filter(|usage| usage.error.is_none())
        .and_then(|usage| usage.secondary_used_percent)
        .is_some_and(|percent| percent.is_finite() && percent >= FULL_WEEKLY_QUOTA_THRESHOLD)
}

fn claim_due_times(
    previous_tick: NaiveDateTime,
    now: NaiveDateTime,
) -> Result<Vec<String>, String> {
    let _guard = SCHEDULE_STORE_LOCK
        .lock()
        .map_err(|_| "Warm-up schedule storage is unavailable".to_string())?;
    let mut store = load_store()?;
    let due = due_times_for_tick(&store, previous_tick, now);
    if due.is_empty() {
        return Ok(due);
    }

    let date = now.format("%Y-%m-%d").to_string();
    for time in &due {
        store.last_fired.insert(time.clone(), date.clone());
    }
    // Claim durably before issuing network requests, so a restart cannot
    // duplicate a schedule that was already dispatched.
    save_store(&store)?;
    Ok(due)
}

fn due_times_for_tick(
    store: &ScheduleStore,
    previous_tick: NaiveDateTime,
    now: NaiveDateTime,
) -> Vec<String> {
    if !store.enabled || store.times.is_empty() {
        return Vec::new();
    }

    let elapsed_ms = (now - previous_tick).num_milliseconds();
    if elapsed_ms <= 0 || elapsed_ms > MAX_TICK_GAP_MS {
        return Vec::new();
    }

    let current_minute = now.format("%H:%M").to_string();
    if previous_tick.format("%Y-%m-%d %H:%M").to_string()
        == now.format("%Y-%m-%d %H:%M").to_string()
    {
        return Vec::new();
    }

    let date = now.format("%Y-%m-%d").to_string();
    if store.last_fired.get(&current_minute) == Some(&date) {
        return Vec::new();
    }

    if store.times.iter().any(|time| time == &current_minute) {
        vec![current_minute]
    } else {
        Vec::new()
    }
}

fn normalize_times(times: &[String]) -> Vec<String> {
    let mut normalized = times
        .iter()
        .filter_map(|time| {
            let (hours, minutes) = time.trim().split_once(':')?;
            let hours = hours.parse::<u8>().ok()?;
            let minutes = minutes.parse::<u8>().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            Some(format!("{hours:02}:{minutes:02}"))
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn schedule_file() -> Result<PathBuf, String> {
    crate::auth::get_config_dir()
        .map(|dir| dir.join(SCHEDULE_FILE_NAME))
        .map_err(|error| error.to_string())
}

fn load_store() -> Result<ScheduleStore, String> {
    let path = schedule_file()?;
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScheduleStore::default());
        }
        Err(error) => {
            return Err(format!("Failed to read {}: {error}", path.display()));
        }
    };

    let mut store: ScheduleStore = serde_json::from_str(&contents)
        .map_err(|error| format!("Failed to parse {}: {error}", path.display()))?;
    store.times = normalize_times(&store.times);
    store.last_fired.retain(|time, date| {
        normalize_times(std::slice::from_ref(time)) == [time.clone()]
            && date.len() == 10
            && date.chars().enumerate().all(|(index, char)| match index {
                4 | 7 => char == '-',
                _ => char.is_ascii_digit(),
            })
    });
    Ok(store)
}

fn save_store(store: &ScheduleStore) -> Result<(), String> {
    let path = schedule_file()?;
    let parent = path
        .parent()
        .ok_or_else(|| "Warm-up schedule path has no parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;

    let temporary_path = path.with_extension("json.tmp");
    let mut file = File::create(&temporary_path)
        .map_err(|error| format!("Failed to create {}: {error}", temporary_path.display()))?;
    serde_json::to_writer_pretty(&mut file, store)
        .map_err(|error| format!("Failed to write {}: {error}", temporary_path.display()))?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Failed to flush {}: {error}", temporary_path.display()))?;
    drop(file);
    fs::rename(&temporary_path, &path)
        .map_err(|error| format!("Failed to replace {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use serde_json::json;

    use super::{
        due_times_for_tick, normalize_times, should_skip_for_full_weekly_quota, ScheduleStore,
    };

    fn local_time(hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 3)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
    }

    fn enabled_store() -> ScheduleStore {
        ScheduleStore {
            enabled: true,
            times: vec!["09:30".to_string()],
            last_fired: Default::default(),
        }
    }

    #[test]
    fn new_schedule_is_off_and_does_not_fire() {
        let mut store = ScheduleStore::default();
        store.times = vec!["09:30".to_string()];
        assert!(!store.enabled);
        assert!(
            due_times_for_tick(&store, local_time(9, 29, 45), local_time(9, 30, 15)).is_empty()
        );
    }

    #[test]
    fn configured_minute_fires_once() {
        let mut store = enabled_store();
        let first = due_times_for_tick(&store, local_time(9, 29, 45), local_time(9, 30, 15));
        assert_eq!(first, vec!["09:30"]);
        store.last_fired.insert("09:30".into(), "2026-10-03".into());
        assert!(
            due_times_for_tick(&store, local_time(9, 30, 15), local_time(9, 30, 45)).is_empty()
        );
    }

    #[test]
    fn persisted_ledger_deduplicates_after_restart() {
        let mut store = enabled_store();
        store.last_fired.insert("09:30".into(), "2026-10-03".into());
        let reopened: ScheduleStore =
            serde_json::from_value(serde_json::to_value(store).unwrap()).unwrap();
        assert!(
            due_times_for_tick(&reopened, local_time(9, 29, 45), local_time(9, 30, 15)).is_empty()
        );
    }

    #[test]
    fn missed_minute_after_sleep_is_not_caught_up() {
        let store = enabled_store();
        assert!(
            due_times_for_tick(&store, local_time(9, 28, 30), local_time(9, 30, 30)).is_empty()
        );
    }

    #[test]
    fn missing_usage_cache_does_not_skip_warmup() {
        assert!(!should_skip_for_full_weekly_quota(None));
        let usage = serde_json::from_value(json!({ "account_id": "test" })).unwrap();
        assert!(!should_skip_for_full_weekly_quota(Some(&usage)));
    }

    #[test]
    fn valid_usage_with_full_weekly_quota_is_skipped() {
        let usage = serde_json::from_value(json!({
            "account_id": "test",
            "secondary_used_percent": 99.5
        }))
        .unwrap();
        assert!(should_skip_for_full_weekly_quota(Some(&usage)));
    }

    #[test]
    fn time_values_are_normalized_and_deduplicated() {
        assert_eq!(
            normalize_times(&["9:5".into(), "09:05".into(), "24:00".into(), "bad".into()]),
            vec!["09:05"]
        );
    }
}
