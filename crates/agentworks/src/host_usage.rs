//! Ephemeral, shared dashboard metrics. No collector exists without a subscriber.
use crate::App;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{Instant, MissedTickBehavior},
};

const SAMPLE_EVERY: Duration = Duration::from_secs(5);
const LEASE: Duration = Duration::from_secs(30);
type Cpu = (u64, u64);
type Sampler = Arc<dyn Fn(&mut Option<Cpu>) -> Value + Send + Sync>;

#[derive(Default)]
pub struct Hub {
    active: Mutex<Option<Active>>,
}
struct Active {
    viewers: usize,
    sample: watch::Receiver<Value>,
    task: tokio::task::AbortHandle,
}
struct Viewer {
    hub: Arc<Hub>,
    sample: watch::Receiver<Value>,
}
impl Drop for Viewer {
    fn drop(&mut self) {
        let mut active = self.hub.active.lock().unwrap();
        if let Some(state) = active.as_mut() {
            state.viewers -= 1;
            if state.viewers == 0 {
                state.task.abort();
                *active = None; // discard the sample and CPU baseline, too
            }
        }
    }
}
impl Hub {
    fn subscribe(self: &Arc<Self>, every: Duration, sampler: Sampler) -> Viewer {
        let mut active = self.active.lock().unwrap();
        let state = active.get_or_insert_with(|| {
            let (tx, rx) =
                watch::channel(json!({"sampledAt":null,"reason":"Waiting for first sample"}));
            let task = tokio::spawn(async move {
                let mut previous = None;
                let mut ticks = tokio::time::interval(every);
                ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    ticks.tick().await;
                    // One read per interval, independent of browser count. Never queue reads.
                    // An already running blocking read may finish after the last viewer leaves;
                    // its result is discarded and no further reads are scheduled.
                    let read = sampler.clone();
                    let Ok((sample, baseline)) = tokio::task::spawn_blocking(move || {
                        let value = read(&mut previous);
                        (value, previous)
                    })
                    .await
                    else {
                        break;
                    };
                    previous = baseline;
                    if tx.send(sample).is_err() {
                        break;
                    }
                }
            });
            Active {
                viewers: 0,
                sample: rx,
                task: task.abort_handle(),
            }
        });
        state.viewers += 1;
        Viewer {
            hub: self.clone(),
            sample: state.sample.clone(),
        }
    }
}

pub async fn events(State(app): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(128)
        .max_frame_size(128)
        .on_upgrade(move |socket| stream(app.host_usage, socket))
}
async fn stream(hub: Arc<Hub>, socket: WebSocket) {
    stream_with(hub, socket, SAMPLE_EVERY, LEASE, Arc::new(sample)).await;
}
async fn stream_with(
    hub: Arc<Hub>,
    socket: WebSocket,
    every: Duration,
    lease: Duration,
    sampler: Sampler,
) {
    let mut viewer = hub.subscribe(every, sampler);
    let (mut sink, mut source) = socket.split();
    let mut deadline = Instant::now() + lease;
    loop {
        let value = viewer.sample.borrow_and_update().clone();
        if !matches!(
            tokio::time::timeout_at(
                deadline.min(Instant::now() + Duration::from_secs(5)),
                sink.send(Message::text(value.to_string()))
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => return,
                incoming = source.next() => match incoming {
                    Some(Ok(Message::Text(text))) if text == "alive" => deadline = Instant::now() + lease,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    _ => return,
                },
                changed = viewer.sample.changed() => {
                    if changed.is_err() { return; }
                    break;
                }
            }
        }
    }
}

fn unavailable(reason: &str) -> Value {
    json!({"reason":reason})
}
fn cpu_counters(text: &str) -> Option<Cpu> {
    let mut fields = text.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    // guest/guest_nice are already included in user/nice; never double count them.
    let values: Vec<u64> = fields
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() != 8 {
        return None;
    }
    let total = values
        .iter()
        .try_fold(0u64, |sum, value| sum.checked_add(*value))?;
    Some((total, values[3].checked_add(values[4])?))
}
fn cpu_percent(previous: Option<Cpu>, current: Cpu) -> Option<f64> {
    let previous = previous?;
    let total = current.0.checked_sub(previous.0)?;
    let idle = current.1.checked_sub(previous.1)?;
    if total == 0 || idle > total {
        return None;
    }
    Some((total - idle) as f64 * 100.0 / total as f64)
}
fn memory(text: &str, total_key: &str, free_key: &str) -> Value {
    let field = |key: &str| -> Option<u64> {
        text.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next()? != key {
                return None;
            }
            let value = parts.next()?.parse::<u64>().ok()?;
            if parts.next()? != "kB" {
                return None;
            }
            value.checked_mul(1024)
        })
    };
    match (field(total_key), field(free_key)) {
        (Some(total), Some(free)) if free <= total => json!({"used":total-free,"total":total}),
        _ => unavailable("Required Linux memory counters unavailable or invalid"),
    }
}
#[cfg(target_os = "linux")]
fn disk() -> Value {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: NUL-terminated constant path and writable statvfs allocation.
    if unsafe { libc::statvfs(c"/".as_ptr(), stat.as_mut_ptr()) } != 0 {
        return unavailable("Root filesystem statistics unavailable");
    }
    // SAFETY: statvfs initialized the structure on success.
    let stat = unsafe { stat.assume_init() };
    let total = (stat.f_blocks as u64).checked_mul(stat.f_frsize as u64);
    let free = (stat.f_bfree as u64).checked_mul(stat.f_frsize as u64);
    match (total, free) {
        (Some(total), Some(free)) if total > 0 && free <= total => {
            json!({"path":"/","used":total-free,"total":total})
        }
        _ => unavailable("Root filesystem counters invalid"),
    }
}
fn sample(previous: &mut Option<Cpu>) -> Value {
    #[cfg(target_os = "linux")]
    {
        let current = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|text| cpu_counters(&text));
        let cpu = match current {
            Some(current) => cpu_percent(*previous, current)
                .map(|percent| json!({"percent":percent}))
                .unwrap_or_else(|| {
                    unavailable(if previous.is_none() {
                        "Warming up: waiting for interval delta"
                    } else {
                        "CPU counters reset or interval invalid"
                    })
                }),
            None => unavailable("Linux CPU counters unavailable or invalid"),
        };
        *previous = current;
        let mem = std::fs::read_to_string("/proc/meminfo");
        let (memory, swap) = match mem {
            Ok(text) => (
                memory(&text, "MemTotal:", "MemAvailable:"),
                memory(&text, "SwapTotal:", "SwapFree:"),
            ),
            Err(_) => (
                unavailable("Linux memory counters unavailable"),
                unavailable("Linux swap counters unavailable"),
            ),
        };
        json!({"sampledAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,"cpu":cpu,"memory":memory,"swap":swap,"disk":disk()})
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = previous;
        json!({"sampledAt":null,"reason":"Host metrics supported only on Linux"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn linux_counter_semantics_and_bad_inputs() {
        assert_eq!(
            cpu_counters("cpu 10 20 30 40 50 60 70 80 900 1000\ncpu0 1"),
            Some((360, 90))
        );
        assert_eq!(cpu_counters("cpu 1 x"), None);
        assert_eq!(cpu_counters("cpu0 1 2 3 4 5 6 7 8"), None);
        assert_eq!(cpu_percent(Some((100, 40)), (200, 65)), Some(75.0));
        for (old, new) in [
            (None, (1, 1)),
            (Some((10, 5)), (10, 5)),
            (Some((10, 5)), (9, 5)),
            (Some((10, 5)), (11, 7)),
        ] {
            assert_eq!(cpu_percent(old, new), None);
        }
        let mem = "MemTotal: 100 kB\nMemAvailable: 25 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n";
        assert_eq!(
            memory(mem, "MemTotal:", "MemAvailable:"),
            json!({"used":76800,"total":102400})
        );
        assert_eq!(
            memory(mem, "SwapTotal:", "SwapFree:"),
            json!({"used":0,"total":0})
        );
        assert!(
            memory(
                "MemTotal: 1 kB\nMemAvailable: 2 kB",
                "MemTotal:",
                "MemAvailable:"
            )["reason"]
                .is_string()
        );
        assert!(memory("", "MemTotal:", "MemAvailable:")["reason"].is_string());
    }
    #[tokio::test]
    async fn shared_collector_stops_and_discards_baseline_after_last_viewer() {
        let hub = Arc::new(Hub::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let sampler: Sampler = Arc::new(move |baseline| {
            count.fetch_add(1, Ordering::SeqCst);
            let warm = baseline.is_none();
            *baseline = Some((1, 1));
            json!({"warm":warm})
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let mut first = hub.subscribe(Duration::from_secs(3600), sampler.clone());
        tokio::time::timeout(Duration::from_secs(2), first.sample.changed())
            .await
            .unwrap()
            .unwrap();
        let second = hub.subscribe(Duration::from_secs(3600), sampler.clone());
        assert_eq!(*first.sample.borrow(), *second.sample.borrow());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let task = hub.active.lock().unwrap().as_ref().unwrap().task.clone();
        drop(first);
        assert_eq!(hub.active.lock().unwrap().as_ref().unwrap().viewers, 1);
        drop(second);
        assert!(hub.active.lock().unwrap().is_none());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let mut fresh = hub.subscribe(Duration::from_secs(3600), sampler);
        assert!(fresh.sample.borrow()["sampledAt"].is_null());
        tokio::time::timeout(Duration::from_secs(2), fresh.sample.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh.sample.borrow()["warm"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn socket_lease_expiry_and_explicit_close_release_collector() {
        use axum::{Router, routing::get};
        use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};
        let hub = Arc::new(Hub::default());
        let serving = hub.clone();
        let router = Router::new().route(
            "/events",
            get(move |ws: WebSocketUpgrade| {
                let hub = serving.clone();
                async move {
                    ws.on_upgrade(move |socket| {
                        stream_with(
                            hub,
                            socket,
                            Duration::from_millis(10),
                            Duration::from_millis(250),
                            Arc::new(|_| json!({"test":true})),
                        )
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut first, _) = connect_async(format!("ws://{address}/events"))
            .await
            .unwrap();
        let (mut second, _) = connect_async(format!("ws://{address}/events"))
            .await
            .unwrap();
        assert_eq!(hub.active.lock().unwrap().as_ref().unwrap().viewers, 2);
        // One viewer explicitly stops; the other eventually stops renewing. Frequent
        // samples must not prevent the independent heartbeat deadline expiring.
        first.send(ClientMessage::Close(None)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if hub
                    .active
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|state| state.viewers == 1)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Renew beyond the original deadline, then stop heartbeats entirely.
        for _ in 0..6 {
            second
                .send(ClientMessage::Text("alive".into()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        assert_eq!(hub.active.lock().unwrap().as_ref().unwrap().viewers, 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while second
                .next()
                .await
                .is_some_and(|message| message.is_ok_and(|m| !m.is_close()))
            {}
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while hub.active.lock().unwrap().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.abort();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_linux_sample_has_freshness_and_root_disk_scope() {
        let value = sample(&mut None);
        assert!(value["sampledAt"].as_u64().is_some());
        assert!(value["cpu"]["reason"].is_string());
        assert_eq!(value["disk"]["path"], "/");
        assert!(value["memory"]["total"].as_u64().unwrap() > 0);
    }
}
