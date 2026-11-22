//! Diagnostic logging shared by the server and remote workers. Lines go to
//! stderr with a UTC timestamp: the service's stderr is its journal, and a
//! worker's stderr travels back over ssh into the server's log, so one log
//! holds both sides of a remote session.
//!
//! `AGENTWORKS_LOG` sets the filter in `RUST_LOG` syntax, such as `debug` or
//! `info,agentworks_runtime=debug`; the default is `info`. A worker without its
//! own setting adopts the server's when it connects.
use std::{sync::OnceLock, time::Instant};
use tracing_subscriber::{
    EnvFilter, Registry, fmt, layer::SubscriberExt, reload, util::SubscriberInitExt,
};

pub const VARIABLE: &str = "AGENTWORKS_LOG";
const DEFAULT: &str = "info";
static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

/// Installs the stderr logger. Later calls, and calls after another logger was
/// installed, do nothing.
pub fn init() {
    let configured = configured();
    let (filter, invalid) = match configured.as_deref().map(EnvFilter::try_new) {
        Some(Ok(filter)) => (filter, None),
        Some(Err(error)) => (EnvFilter::new(DEFAULT), Some(error)),
        None => (EnvFilter::new(DEFAULT), None),
    };
    let (filter, handle) = reload::Layer::new(filter);
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
        .try_init();
    if installed.is_ok() {
        let _ = FILTER.set(handle);
        if let Some(error) = invalid {
            tracing::warn!("{VARIABLE} is invalid, using {DEFAULT}: {error}");
        }
    }
}

/// This process's own filter setting, which the server forwards to workers.
pub fn configured() -> Option<String> {
    std::env::var(VARIABLE)
        .ok()
        .filter(|spec| !spec.trim().is_empty())
}

/// Adopts the server's filter unless this process has its own setting.
pub fn adopt(spec: &str) {
    if configured().is_some() {
        return;
    }
    let Some(handle) = FILTER.get() else { return };
    match EnvFilter::try_new(spec) {
        Ok(filter) => {
            let _ = handle.reload(filter);
        }
        Err(error) => tracing::warn!("Ignoring the server's log filter {spec:?}: {error}"),
    }
}

/// Whole milliseconds since `start`, for `elapsed_ms` fields.
pub fn ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}
