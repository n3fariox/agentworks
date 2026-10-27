//! A loopback callback listener owned by one pending browser sign-in.
use super::*;
use std::{future::IntoFuture, net::Ipv4Addr, time::Duration};
use tokio::{net::TcpListener, sync::oneshot};

pub(super) const REDIRECT: &str = "http://localhost:1455/auth/callback";
pub(super) struct Binding {
    profile: String,
    shutdown: oneshot::Sender<()>,
    released: oneshot::Receiver<()>,
}
impl Transactions {
    pub(super) async fn close_browser(&mut self, id: &str) {
        if self
            .browser
            .as_ref()
            .is_some_and(|binding| binding.profile == id)
        {
            let binding = self.browser.take().unwrap();
            let _ = binding.shutdown.send(());
            // Wait for the listening socket to drop before another attempt can
            // bind. Already accepted callbacks can still finish their response.
            let _ = binding.released.await;
        }
    }
    pub(super) async fn bind_browser(
        &mut self,
        id: &str,
        state: &str,
        lifetime: Duration,
    ) -> Result<(), (StatusCode, String)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 1455)).await.map_err(|_| {
            fail(StatusCode::CONFLICT, "Browser sign-in needs localhost:1455, which is already in use. Finish the other login or choose Device code.")
        })?;
        let (shutdown, stop) = oneshot::channel();
        let (release, released) = oneshot::channel();
        self.browser = Some(Binding {
            profile: id.to_owned(),
            shutdown,
            released,
        });
        let id = id.to_owned();
        let state = state.to_owned();
        tokio::spawn(async move {
            let app = axum::Router::new().route("/auth/callback", axum::routing::get(callback));
            let expired = {
                // Dropping Serve releases the listening socket immediately;
                // accepted requests run on their own connection tasks.
                let serving = axum::serve(listener, app).into_future();
                tokio::select! {
                    _ = stop => false,
                    _ = tokio::time::sleep(lifetime) => true,
                    _ = serving => true,
                }
            };
            let _ = release.send(());
            if expired {
                let mut transactions = TRANSACTIONS.lock().await;
                if transactions
                    .status
                    .get(&id)
                    .is_some_and(|status| status.state == state && status.phase == "pending")
                {
                    transactions.pending.remove(&state);
                    let status = transactions.status.get_mut(&id).unwrap();
                    status.phase = "error";
                    status.error = Some("Sign-in expired. Try again.".into());
                }
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_browser_listener_times_out_without_polling() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut transactions = TRANSACTIONS.lock().await;
        transactions.status.insert(
            id.clone(),
            Status {
                state: "timeout-state".into(),
                phase: "pending",
                error: None,
                expires: credentials::now() + 600,
            },
        );
        transactions
            .bind_browser(&id, "timeout-state", Duration::from_millis(20))
            .await
            .unwrap();
        drop(transactions);
        tokio::time::sleep(Duration::from_millis(80)).await;
        let available = TcpListener::bind((Ipv4Addr::LOCALHOST, 1455))
            .await
            .unwrap();
        let mut transactions = TRANSACTIONS.lock().await;
        assert_eq!(transactions.status[&id].phase, "error");
        assert!(
            transactions.status[&id]
                .error
                .as_deref()
                .unwrap()
                .contains("expired")
        );
        transactions.cancel(&id).await;
        drop(available);
    }
}
