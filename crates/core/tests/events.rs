//! The change watcher: bursts are folded, a broken stream only delays the
//! frontend, and the mock backend announces its own changes (design 3.4).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hypervctl_backend_mock::MockBackend;
use hypervctl_core::{
    AttachedVhd, BackendActor, BackendHandle, ChangeEvent, ChangeWatcher, Checkpoint, CheckpointId,
    HyperV, HyperVError, JobRef, JobStatus, Pending, RawError, RequestedState, SwitchDetail,
    SwitchId, SwitchSummary, Thumbnail, VhdInfo, VmDetail, VmId, VmSummary, DEBOUNCE,
    RECONNECT_DELAY,
};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::mpsc::error::TryRecvError;

type Stream = tokio_mpsc::UnboundedReceiver<ChangeEvent>;

/// Hands out one notification stream per subscription attempt, in the order the
/// test queued them, and counts every attempt including the failed ones.
struct StreamingBackend {
    queue: std_mpsc::Receiver<Stream>,
    attempts: Arc<AtomicUsize>,
}

fn unused() -> HyperVError {
    HyperVError::backend(RawError::new("not exercised by this test"))
}

impl HyperV for StreamingBackend {
    fn subscribe_changes(&mut self) -> Result<Stream, HyperVError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        match self.queue.try_recv() {
            Ok(stream) => Ok(stream),
            Err(std_mpsc::TryRecvError::Empty) => {
                Err(HyperVError::backend(RawError::new("no stream queued yet")))
            }
            Err(std_mpsc::TryRecvError::Disconnected) => Err(unused()),
        }
    }

    fn list_vms(&mut self) -> Result<Vec<VmSummary>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_thumbnail(
        &mut self,
        _id: VmId,
        _width: u16,
        _height: u16,
    ) -> Result<Thumbnail, HyperVError> {
        Err(unused())
    }

    fn get_vm(&mut self, _id: VmId) -> Result<VmDetail, HyperVError> {
        Err(unused())
    }

    fn list_checkpoints(&mut self, _vm: VmId) -> Result<Vec<Checkpoint>, HyperVError> {
        Ok(Vec::new())
    }

    fn list_switches(&mut self) -> Result<Vec<SwitchSummary>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_switch(&mut self, _id: SwitchId) -> Result<SwitchDetail, HyperVError> {
        Err(unused())
    }

    fn list_vhds(&mut self, _vm: Option<VmId>) -> Result<Vec<AttachedVhd>, HyperVError> {
        Ok(Vec::new())
    }

    fn get_vhd(&mut self, _path: &str) -> Result<VhdInfo, HyperVError> {
        Err(unused())
    }

    fn request_vm_state(
        &mut self,
        _id: VmId,
        _request: RequestedState,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn request_guest_reboot(&mut self, _id: VmId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn create_checkpoint(&mut self, _vm: VmId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn rename_checkpoint(
        &mut self,
        _checkpoint: CheckpointId,
        _name: &str,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn restore_checkpoint(
        &mut self,
        _vm: VmId,
        _checkpoint: CheckpointId,
    ) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn remove_checkpoint(&mut self, _checkpoint: CheckpointId) -> Result<Pending, HyperVError> {
        Err(unused())
    }

    fn job_status(&mut self, _job: JobRef) -> Result<JobStatus, HyperVError> {
        Err(unused())
    }
}

/// A handle to a backend whose streams the test hands out one by one.
fn streaming_backend() -> (BackendHandle, std_mpsc::Sender<Stream>, Arc<AtomicUsize>) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let (queue, streams) = std_mpsc::channel();
    let counter = attempts.clone();
    let handle = BackendActor::spawn(move || StreamingBackend {
        queue: streams,
        attempts: counter,
    });
    (handle, queue, attempts)
}

/// Lets the watcher run and fires whatever reconnect delay it is waiting for.
///
/// The backend answers from a thread of its own, and no clock the test drives
/// can hurry that thread along. The wait therefore spins until the attempt
/// shows up, bounded by the *real* clock, and steps the paused clock forward
/// for the timers in between. Spinning also keeps the runtime from parking:
/// a parked runtime would jump the paused clock straight past any timeout the
/// test is still waiting on, which is what makes these tests flaky under load.
async fn pump(attempts: &AtomicUsize, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while attempts.load(Ordering::SeqCst) < want {
        assert!(
            Instant::now() < deadline,
            "the watcher must reach {want} subscription attempts, has {}",
            attempts.load(Ordering::SeqCst)
        );
        tokio::task::yield_now().await;
        tokio::time::advance(RECONNECT_DELAY).await;
    }
}

/// Takes one notification off the watcher, stepping the debounce forward
/// instead of waiting for a timer that a paused clock would fire at once.
async fn next_event(received: &mut Stream) -> ChangeEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match received.try_recv() {
            Ok(event) => return event,
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => panic!("the watcher must keep its output open"),
        }
        assert!(
            Instant::now() < deadline,
            "no notification arrived within ten seconds"
        );
        tokio::task::yield_now().await;
        tokio::time::advance(DEBOUNCE).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_the_same_notification_arrives_once() {
    let (handle, queue, attempts) = streaming_backend();
    let (stream_tx, stream_rx) = tokio_mpsc::unbounded_channel();
    assert!(queue.send(stream_rx).is_ok());
    let (output, mut received) = tokio_mpsc::unbounded_channel();
    let _watcher = ChangeWatcher::spawn(handle, output);

    pump(&attempts, 1).await;
    let _ = stream_tx.send(ChangeEvent::Vms);
    let _ = stream_tx.send(ChangeEvent::Vms);
    let _ = stream_tx.send(ChangeEvent::Vms);

    assert_eq!(
        next_event(&mut received).await,
        ChangeEvent::Vms,
        "expected one Vms notification"
    );
    assert!(
        received.try_recv().is_err(),
        "the burst must be folded into a single notification"
    );
}

#[tokio::test(start_paused = true)]
async fn notifications_that_differ_are_not_merged() {
    let (handle, queue, attempts) = streaming_backend();
    let (stream_tx, stream_rx) = tokio_mpsc::unbounded_channel();
    assert!(queue.send(stream_rx).is_ok());
    let (output, mut received) = tokio_mpsc::unbounded_channel();
    let _watcher = ChangeWatcher::spawn(handle, output);

    pump(&attempts, 1).await;
    let vm = VmId::new(uuid::Uuid::from_u128(7));
    let _ = stream_tx.send(ChangeEvent::Vms);
    let _ = stream_tx.send(ChangeEvent::Checkpoints(vm));

    assert_eq!(
        next_event(&mut received).await,
        ChangeEvent::Vms,
        "expected Vms first"
    );
    match next_event(&mut received).await {
        ChangeEvent::Checkpoints(found) => assert_eq!(found, vm),
        other => panic!("expected the checkpoint notification, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn the_watcher_subscribes_again_after_its_stream_ends() {
    // Acceptance test 13: until the stream is back the frontend keeps working
    // by polling, so a lost subscription is an inconvenience, not an outage.
    let (handle, queue, attempts) = streaming_backend();
    let (first_tx, first_rx) = tokio_mpsc::unbounded_channel();
    assert!(queue.send(first_rx).is_ok());
    let (output, mut received) = tokio_mpsc::unbounded_channel();
    let _watcher = ChangeWatcher::spawn(handle, output);

    pump(&attempts, 1).await;
    let _ = first_tx.send(ChangeEvent::Vms);
    assert_eq!(
        next_event(&mut received).await,
        ChangeEvent::Vms,
        "expected the first notification"
    );

    // The stream ends and nothing is queued: the watcher notices and retries.
    drop(first_tx);
    pump(&attempts, 2).await;
    let after_disconnect = attempts.load(Ordering::SeqCst);
    assert!(
        after_disconnect >= 2,
        "the watcher must subscribe again, got {after_disconnect} attempts"
    );

    // A new stream shows up; it is picked up without any other intervention.
    let (second_tx, second_rx) = tokio_mpsc::unbounded_channel();
    assert!(queue.send(second_rx).is_ok());
    pump(&attempts, after_disconnect + 1).await;
    let vm = VmId::new(uuid::Uuid::from_u128(9));
    assert!(
        second_tx.send(ChangeEvent::Checkpoints(vm)).is_ok(),
        "the watcher must be listening on the new stream"
    );
    match next_event(&mut received).await {
        ChangeEvent::Checkpoints(found) => assert_eq!(found, vm),
        other => panic!("expected a notification on the new stream, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn the_mock_backend_announces_its_own_changes() {
    let mut backend = MockBackend::with_fixture();
    // A transition with no delay settles on the next call, so the test never
    // waits for a clock it cannot drive (the mock lives on the actor thread).
    backend.set_transition_delay(Duration::ZERO);
    let handle = BackendActor::spawn(move || backend);
    let mut stream = match handle.subscribe_changes().await {
        Ok(stream) => stream,
        Err(error) => panic!("expected a subscription, got {error}"),
    };
    let vms = match handle.list_vms().await {
        Ok(vms) => vms,
        Err(error) => panic!("expected the fixture, got {error}"),
    };
    let web02 = match vms.iter().find(|vm| vm.name == "web-02") {
        Some(vm) => vm.id,
        None => panic!("the fixture must contain web-02"),
    };

    if let Err(error) = handle.request_vm_state(web02, RequestedState::Start).await {
        panic!("expected the request to be accepted, got {error}");
    }
    assert_eq!(
        next_event(&mut stream).await,
        ChangeEvent::Vms,
        "the request must be announced"
    );

    // The transition settling is announced too.
    match handle.list_vms().await {
        Ok(_) => {}
        Err(error) => panic!("expected the list, got {error}"),
    }
    assert_eq!(
        next_event(&mut stream).await,
        ChangeEvent::Vms,
        "the settled transition must be announced"
    );
}
