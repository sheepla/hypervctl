//! Change notifications: what changed, folded into bursts, and how the stream
//! survives losing the watcher (design 3.4).

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::handle::BackendHandle;
use crate::ids::VmId;

/// One notification. It deliberately carries no payload: the receiver re-reads
/// whatever it needs with the ordinary APIs, so a dropped or late event never
/// turns into stale data (design 3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeEvent {
    /// VMs were added, removed or changed state.
    Vms,
    /// The checkpoint set of one VM changed.
    Checkpoints(VmId),
}

/// How long a burst of notifications is folded into one (design 3.4).
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// How long to wait before subscribing again after a broken stream (design 3.4).
pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// Forwards the backend's notifications to `output`, coalescing bursts and
/// re-subscribing whenever the stream ends.
pub struct ChangeWatcher;

impl ChangeWatcher {
    /// Runs on its own task until `output` is dropped.
    ///
    /// A stream that breaks only delays the frontend: the watcher subscribes
    /// again after [`RECONNECT_DELAY`] and keeps working in between, because
    /// polling the ordinary APIs is enough to stay usable (design 3.4).
    pub fn spawn(
        handle: BackendHandle,
        output: mpsc::UnboundedSender<ChangeEvent>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match handle.subscribe_changes().await {
                    Ok(stream) => {
                        if forward(stream, &output).await == End::OutputClosed {
                            break;
                        }
                        tracing::warn!(
                            "change notifications ended; re-subscribing in {RECONNECT_DELAY:?}"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            "change notifications unavailable: {error}; re-subscribing in {RECONNECT_DELAY:?}"
                        );
                    }
                }
                if wait_or_close(&output, RECONNECT_DELAY).await.is_err() {
                    break;
                }
            }
        })
    }
}

/// Why [`forward`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Nobody is listening any more.
    OutputClosed,
    /// The backend's stream ended; re-subscribe.
    Disconnected,
}

/// Sends what arrives until the stream ends or the output closes.
async fn forward(
    mut stream: mpsc::UnboundedReceiver<ChangeEvent>,
    output: &mpsc::UnboundedSender<ChangeEvent>,
) -> End {
    loop {
        let first = match stream.recv().await {
            Some(event) => event,
            None => return End::Disconnected,
        };
        let mut batch = vec![first];
        loop {
            match tokio::time::timeout(DEBOUNCE, stream.recv()).await {
                Ok(Some(event)) => {
                    if !batch.contains(&event) {
                        batch.push(event);
                    }
                }
                Ok(None) => {
                    // Quiet because the stream ended, not because time ran out.
                    let end = send_batch(&batch, output).await;
                    return match end {
                        Ok(()) => End::Disconnected,
                        Err(()) => End::OutputClosed,
                    };
                }
                Err(_) => break,
            }
        }
        if send_batch(&batch, output).await.is_err() {
            return End::OutputClosed;
        }
    }
}

async fn send_batch(
    batch: &[ChangeEvent],
    output: &mpsc::UnboundedSender<ChangeEvent>,
) -> Result<(), ()> {
    for event in batch {
        if output.send(event.clone()).is_err() {
            return Err(());
        }
    }
    Ok(())
}

/// Waits out the reconnect delay, giving up if the output closes meanwhile.
async fn wait_or_close(
    output: &mpsc::UnboundedSender<ChangeEvent>,
    delay: Duration,
) -> Result<(), ()> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = output.closed() => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_and_reconnect_are_the_documented_values() {
        assert_eq!(DEBOUNCE, Duration::from_millis(100));
        assert_eq!(RECONNECT_DELAY, Duration::from_secs(5));
        assert_ne!(
            ChangeEvent::Vms,
            ChangeEvent::Checkpoints(VmId::new(uuid::Uuid::nil()))
        );
    }

    #[test]
    fn a_batch_folds_duplicates_only() {
        let vm = VmId::new(uuid::Uuid::nil());
        let mut batch = vec![ChangeEvent::Vms];
        for event in [
            ChangeEvent::Vms,
            ChangeEvent::Checkpoints(vm),
            ChangeEvent::Vms,
        ] {
            if !batch.contains(&event) {
                batch.push(event);
            }
        }
        assert_eq!(batch.len(), 2);
    }
}
