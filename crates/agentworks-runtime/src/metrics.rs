//! Read-only host metrics, shared by the local dashboard and SSH workers.
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{DiskRefreshKind, Disks, System};

pub const EVERY: Duration = Duration::from_secs(5);
pub const LEASE: Duration = Duration::from_secs(30);
pub const MAX_EVENT_BYTES: usize = 64 * 1024;
pub const MAX_DISKS: usize = 64;

pub fn supported() -> bool {
    sysinfo::IS_SUPPORTED_SYSTEM
        && cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        ))
}
pub fn unavailable(reason: &str) -> Value {
    json!({"reason":reason})
}
#[derive(Default)]
pub struct Collector {
    system: System,
    previous: Option<Instant>,
}
impl Collector {
    pub fn sample(&mut self) -> Value {
        if !supported() {
            return unavailable("Host metrics support Linux, macOS, and Windows only");
        }
        self.system.refresh_cpu_usage();
        // Fresh memory state avoids retaining old capacity after a failed refresh.
        let mut memory = System::new();
        memory.refresh_memory();
        let now = Instant::now();
        let cpu = if self.system.cpus().is_empty() {
            self.previous = None;
            unavailable("CPU counters unavailable")
        } else {
            let ready = self
                .previous
                .is_some_and(|old| now.duration_since(old) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            self.previous = Some(now);
            cpu_value(ready, self.system.global_cpu_usage())
        };
        // Recreate disk snapshots so a failed OS refresh cannot retain an old
        // capacity under a fresh timestamp. Refresh storage only, never disk I/O.
        let mut disks = Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing());
        let mut candidates = std::collections::BTreeMap::new();
        let mut truncated = false;
        for (index, disk) in disks.list().iter().enumerate() {
            let path = disk.mount_point().to_string_lossy().into_owned();
            let fs = disk.file_system().to_string_lossy();
            if !interesting(&path, &fs, disk.is_removable()) {
                continue;
            }
            if path.len() > 4096 {
                truncated = true;
                continue;
            }
            candidates.entry(path).or_insert(index);
        }
        truncated |= candidates.len() > MAX_DISKS;
        let mut selected = Vec::new();
        // Bound actual capacity reads, not just the size of the emitted list.
        for (path, index) in candidates.into_iter().take(MAX_DISKS) {
            let disk = &mut disks.list_mut()[index];
            let refreshed = disk.refresh_specifics(DiskRefreshKind::nothing().with_storage());
            let mut value = match disk.total_space().checked_sub(disk.available_space()) {
                Some(used) if refreshed => capacity(used, disk.total_space(), false),
                _ => unavailable("OS disk capacity unavailable or invalid"),
            };
            value["path"] = json!(path);
            value["fileSystem"] = json!(disk.file_system().to_string_lossy());
            selected.push(value);
        }
        let mut disks = selected;
        while serde_json::to_vec(&disks).unwrap_or_default().len() > MAX_EVENT_BYTES - 4096 {
            disks.pop();
            truncated = true;
        }
        json!({
            "sampledAt":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            "cpu":cpu,
            "memory":capacity(memory.used_memory(), memory.total_memory(), false),
            "swap":capacity(memory.used_swap(), memory.total_swap(), true),
            "disks":disks,
            "diskReason":if disks.is_empty() { Some("No readable local storage volumes reported") } else { None },
            "disksTruncated":truncated,
        })
    }
}
fn cpu_value(ready: bool, percent: f32) -> Value {
    if !ready {
        unavailable("Warming up: waiting for interval delta")
    } else if percent.is_finite() && (0.0..=100.0).contains(&percent) {
        json!({"percent":percent})
    } else {
        unavailable("CPU interval counters invalid")
    }
}
fn capacity(used: u64, total: u64, zero_valid: bool) -> Value {
    if used > total || (total == 0 && !zero_valid) {
        unavailable("OS capacity counters unavailable or invalid")
    } else {
        json!({"used":used,"total":total})
    }
}
fn interesting(path: &str, fs: &str, removable: bool) -> bool {
    !removable
        && !path.is_empty()
        && !fs.starts_with("fuse")
        && !matches!(
            fs,
            "proc"
                | "sysfs"
                | "tmpfs"
                | "devtmpfs"
                | "cgroup"
                | "cgroup2"
                | "devpts"
                | "squashfs"
                | "iso9660"
                | "nfs"
                | "nfs4"
                | "cifs"
                | "smbfs"
                | "autofs"
                | "fuse.sshfs"
        )
        && !["/proc", "/sys", "/dev", "/run", "/boot", "/efi"]
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

/// One worker-local collector and one lease. Duplicate starts renew, without
/// replacing the CPU baseline. A new subscription supersedes the old one.
#[derive(Default)]
pub(crate) struct WorkerMetrics {
    active: Option<WorkerActive>,
}
struct WorkerActive {
    id: u64,
    renewal: tokio::sync::watch::Sender<tokio::time::Instant>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for WorkerActive {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl WorkerMetrics {
    pub(crate) fn start(
        &mut self,
        id: u64,
        out: tokio::sync::mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    ) {
        if id == 0 {
            return;
        }
        if let Some(active) = &self.active {
            if active.id == id && !active.task.is_finished() {
                active
                    .renewal
                    .send_replace(tokio::time::Instant::now() + LEASE);
                return;
            }
        }
        self.active = None;
        let (renewal, mut renewed) =
            tokio::sync::watch::channel(tokio::time::Instant::now() + LEASE);
        let task = tokio::spawn(async move {
            let mut collector = Collector::default();
            let mut ticks = tokio::time::interval(EVERY);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let deadline = *renewed.borrow_and_update();
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => break,
                    result = renewed.changed() => { if result.is_err() { break; } },
                    _ = ticks.tick() => {
                        let read = tokio::task::spawn_blocking(move || { let sample = collector.sample(); (collector, sample) });
                        let result = tokio::time::timeout_at(deadline, read).await;
                        let Ok(Ok((state, sample))) = result else { break; };
                        collector = state;
                        let event = json!({"type":"metricsEvent","subscription":id,"sample":sample}).to_string();
                        if event.len() > MAX_EVENT_BYTES || out.try_send(tokio_tungstenite::tungstenite::Message::text(event)).is_err() { break; }
                    }
                }
            }
        });
        self.active = Some(WorkerActive { id, renewal, task });
    }
    pub(crate) fn stop(&mut self, id: u64) {
        if self.active.as_ref().is_some_and(|active| active.id == id) {
            self.active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_validate_and_select_storage_mounts() {
        assert!(cpu_value(false, 50.0)["reason"].is_string());
        assert!(cpu_value(true, f32::NAN)["reason"].is_string());
        assert_eq!(cpu_value(true, 25.0)["percent"], 25.0);
        assert_eq!(capacity(0, 0, true), json!({"used":0,"total":0}));
        assert!(capacity(2, 1, false)["reason"].is_string());
        assert!(interesting("/mnt/build", "ext4", false));
        assert!(interesting("/", "btrfs", false));
        assert!(interesting("/boot-build", "ext4", false));
        assert!(interesting("D:\\", "NTFS", false));
        assert!(interesting("/Volumes/build", "apfs", false));
        for (path, fs, removable) in [
            ("/proc", "proc", false),
            ("/boot", "ext4", false),
            ("/boot/efi", "vfat", false),
            ("/efi", "vfat", false),
            ("/efi/EFI", "vfat", false),
            ("/mnt/nfs", "nfs", false),
            ("/media/usb", "ext4", true),
            ("/run/something", "ext4", false),
        ] {
            assert!(!interesting(path, fs, removable));
        }
    }
    #[tokio::test]
    async fn worker_start_renews_stop_fences_ids_and_expiry_discards_collector() {
        let (out, mut events) = tokio::sync::mpsc::channel(8);
        let mut worker = WorkerMetrics::default();
        assert!(worker.active.is_none());
        worker.start(7, out.clone());
        let first = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let first: Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
        assert_eq!(first["subscription"], 7);
        assert!(valid_sample(&first["sample"]));
        let task = worker.active.as_ref().unwrap().task.id();
        worker.start(7, out.clone());
        assert_eq!(worker.active.as_ref().unwrap().task.id(), task);
        worker.stop(6);
        assert!(worker.active.is_some());
        worker.stop(7);
        assert!(worker.active.is_none());
        worker.start(8, out.clone());
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
        assert_eq!(event["subscription"], 8);
        assert!(event["sample"]["cpu"]["reason"].is_string());
        worker
            .active
            .as_ref()
            .unwrap()
            .renewal
            .send_replace(tokio::time::Instant::now() + Duration::from_millis(20));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !worker.active.as_ref().unwrap().task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        worker.start(8, out.clone());
        assert!(!worker.active.as_ref().unwrap().task.is_finished());
        let aborted = worker.active.as_ref().unwrap().task.abort_handle();
        drop(worker);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !aborted.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[test]
    fn worker_events_reject_invalid_shapes_and_large_mount_lists() {
        assert!(!valid_sample(&json!({"cpu":{"percent":"hello"}})));
        let mut sample = json!({"sampledAt":1,"cpu":{"percent":10},"memory":{"used":1,"total":2},"swap":{"used":0,"total":0},"disks":[{"path":"/build","used":1,"total":2}]});
        assert!(valid_sample(&sample));
        sample["disks"] = json!(vec![sample["disks"][0].clone(); MAX_DISKS + 1]);
        assert!(!valid_sample(&sample));
        sample["disks"] = json!([]);
        sample["cpu"]["percent"] = json!(101);
        assert!(!valid_sample(&sample));
        sample["cpu"] = json!({"reason":"unavailable","percent":"bad"});
        assert!(!valid_sample(&sample));
    }
    #[test]
    fn native_sample_warms_up_and_contains_no_processes() {
        if !supported() {
            return;
        }
        let mut collector = Collector::default();
        let sample = collector.sample();
        assert!(sample["sampledAt"].is_u64());
        assert!(sample["cpu"]["reason"].is_string());
        assert!(sample["disks"].is_array());
        assert!(sample.get("processes").is_none());
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let next = collector.sample();
        assert!(next["cpu"]["percent"].is_number());
        assert!(valid_sample(&next));
    }
}

/// Treat worker events as untrusted input. Bound their shape before forwarding
/// them to browsers or replacing the current observation.
pub fn valid_sample(value: &Value) -> bool {
    let reason = |value: &Value| {
        value["reason"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 512)
    };
    let capacity = |v: &Value| {
        if v.get("total").is_some() {
            match (v["used"].as_u64(), v["total"].as_u64()) {
                (Some(u), Some(t)) => u <= t,
                _ => false,
            }
        } else {
            reason(v)
        }
    };
    if reason(value) {
        return true;
    }
    value["sampledAt"]
        .as_u64()
        .is_some_and(|at| at <= 8_640_000_000_000_000)
        && (if value["cpu"].get("percent").is_some() {
            value["cpu"]["percent"]
                .as_f64()
                .is_some_and(|v| v.is_finite() && (0.0..=100.0).contains(&v))
        } else {
            reason(&value["cpu"])
        })
        && capacity(&value["memory"])
        && capacity(&value["swap"])
        && value["disks"].as_array().is_some_and(|disks| {
            disks.len() <= MAX_DISKS
                && disks.iter().all(|disk| {
                    capacity(disk)
                        && disk["path"]
                            .as_str()
                            .is_some_and(|p| !p.is_empty() && p.len() <= 4096)
                })
        })
}
