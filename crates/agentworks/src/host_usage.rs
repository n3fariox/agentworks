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

const SAMPLE_EVERY: Duration = agentworks_runtime::metrics::EVERY;
const LEASE: Duration = agentworks_runtime::metrics::LEASE;
#[derive(Default)]
struct Sampling {
    local: agentworks_runtime::metrics::Collector,
}
#[derive(Default)]
struct RemoteSampling {
    stopped: std::sync::atomic::AtomicBool,
    subscriptions:
        Mutex<std::collections::HashMap<String, agentworks_runtime::remote::MetricsSubscription>>,
}
impl RemoteSampling {
    fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.subscriptions.lock().unwrap().clear();
    }
}
type Sampler = Arc<dyn Fn(&mut Option<Sampling>) -> Value + Send + Sync>;

#[derive(Default)]
pub struct Hub {
    active: Mutex<Option<Active>>,
    shutdown: tokio_util::sync::CancellationToken,
}
struct Active {
    viewers: usize,
    sample: watch::Receiver<Value>,
    task: tokio::task::AbortHandle,
    stop: Option<Arc<dyn Fn() + Send + Sync>>,
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
                if let Some(stop) = &state.stop {
                    stop();
                }
                *active = None; // discard the sample and CPU baseline, too
            }
        }
    }
}
impl Hub {
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
    fn subscribe(
        self: &Arc<Self>,
        every: Duration,
        sampler: Sampler,
        stop: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Viewer {
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
                stop,
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
        .on_upgrade(move |socket| {
            let hosts = app.hosts;
            let catalog = app.catalog;
            let remote = Arc::new(RemoteSampling::default());
            let stopping = remote.clone();
            stream_with(
                app.host_usage,
                socket,
                SAMPLE_EVERY,
                LEASE,
                Arc::new(move |state| sample(&hosts, &catalog, &remote, state)),
                Some(Arc::new(move || stopping.stop())),
            )
        })
}
async fn stream_with(
    hub: Arc<Hub>,
    socket: WebSocket,
    every: Duration,
    lease: Duration,
    sampler: Sampler,
    stop: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    if hub.shutdown.is_cancelled() {
        return;
    }
    let shutdown = hub.shutdown.clone();
    let mut viewer = hub.subscribe(every, sampler, stop);
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
                _ = shutdown.cancelled() => return,
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

fn sample(
    hosts: &crate::hosts::Hosts,
    catalog: &Mutex<crate::catalog::Catalog>,
    remote: &RemoteSampling,
    state: &mut Option<Sampling>,
) -> Value {
    use agentworks_runtime::metrics::unavailable;
    let state = state.get_or_insert_with(Sampling::default);
    let mut samples = serde_json::Map::new();
    samples.insert("local".into(), state.local.sample());
    let configured = catalog.lock().unwrap().data.hosts.clone();
    let mut subscriptions = remote.subscriptions.lock().unwrap();
    if remote.stopped.load(std::sync::atomic::Ordering::Acquire) {
        return json!({"hosts":{}});
    }
    subscriptions.retain(|id, _| configured.iter().any(|host| host.id == *id));
    for host in configured {
        let value = match hosts.live(&host.id) {
            None => {
                subscriptions.remove(&host.id);
                unavailable("Remote host disconnected")
            }
            Some(live) => {
                if subscriptions
                    .get(&host.id)
                    .is_some_and(|subscription| !subscription.same_connection(&live))
                {
                    subscriptions.remove(&host.id); // reset baseline on reconnect
                }
                if !subscriptions.contains_key(&host.id) {
                    match live.metrics() {
                        Ok(subscription) => {
                            subscriptions.insert(host.id.clone(), subscription);
                        }
                        Err(reason) => {
                            samples.insert(host.id, unavailable(&reason));
                            continue;
                        }
                    }
                }
                match subscriptions[&host.id].latest() {
                    Ok(value) => value,
                    Err(reason) => {
                        subscriptions.remove(&host.id);
                        unavailable(&reason)
                    }
                }
            }
        };
        samples.insert(host.id, value);
    }
    json!({"hosts":samples})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn shared_collector_stops_and_discards_baseline_after_last_viewer() {
        let hub = Arc::new(Hub::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let sampler: Sampler = Arc::new(move |baseline| {
            count.fetch_add(1, Ordering::SeqCst);
            let warm = baseline.is_none();
            *baseline = Some(Sampling::default());
            json!({"warm":warm})
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let stops = Arc::new(AtomicUsize::new(0));
        let stopped = stops.clone();
        let mut first = hub.subscribe(
            Duration::from_secs(3600),
            sampler.clone(),
            Some(Arc::new(move || {
                stopped.fetch_add(1, Ordering::SeqCst);
            })),
        );
        tokio::time::timeout(Duration::from_secs(2), first.sample.changed())
            .await
            .unwrap()
            .unwrap();
        let second = hub.subscribe(Duration::from_secs(3600), sampler.clone(), None);
        assert_eq!(*first.sample.borrow(), *second.sample.borrow());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let task = hub.active.lock().unwrap().as_ref().unwrap().task.clone();
        drop(first);
        assert_eq!(hub.active.lock().unwrap().as_ref().unwrap().viewers, 1);
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        drop(second);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        assert!(hub.active.lock().unwrap().is_none());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let mut fresh = hub.subscribe(Duration::from_secs(3600), sampler, None);
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
                            None,
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
        let (mut third, _) = connect_async(format!("ws://{address}/events"))
            .await
            .unwrap();
        hub.shutdown();
        tokio::time::timeout(Duration::from_secs(2), async {
            while third
                .next()
                .await
                .is_some_and(|message| message.is_ok_and(|m| !m.is_close()))
            {}
            while hub.active.lock().unwrap().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.abort();
    }
}
