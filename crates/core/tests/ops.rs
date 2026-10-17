//! The operation layer against the in-memory backend: waiting, idempotency,
//! target resolution and restart (design 3.3, spec 2.6-2.9).
//!
//! Every test runs on a paused clock, so no test ever waits in real time.

use std::time::Duration;

use hypervctl_backend_mock::{MockBackend, TickMode};
use hypervctl_core::{
    BackendActor, HyperVError, JobFailReason, JobRef, JobStatus, OpOutcome, OpSuccess, Operations,
    Pending, Phase, Progress, RawError, TargetKind, VmOp, VmState, WaitPolicy,
};

fn specs(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// The standard fixture, with transitions that settle on the very next call.
/// The mock runs on the actor thread, which cannot see a paused test clock, so
/// a test that must wait for a transition settles it with a zero delay instead.
fn fixture() -> MockBackend {
    let mut backend = MockBackend::with_fixture();
    backend.set_transition_delay(Duration::ZERO);
    backend
}

async fn operations(backend: MockBackend) -> Operations {
    Operations::new(BackendActor::spawn(move || backend))
}

fn first(outcomes: &[OpOutcome]) -> &OpOutcome {
    match outcomes.first() {
        Some(outcome) => outcome,
        None => panic!("expected at least one outcome"),
    }
}

fn success(outcome: &OpOutcome) -> &OpSuccess {
    match &outcome.result {
        Ok(success) => success,
        Err(error) => panic!("expected '{}' to succeed, got {error}", outcome.target),
    }
}

fn failure(outcome: &OpOutcome) -> &HyperVError {
    match &outcome.result {
        Err(error) => error,
        Ok(success) => panic!("expected '{}' to fail, got {success:?}", outcome.target),
    }
}

fn ok<T>(result: Result<T, HyperVError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("expected success, got {error}"),
    }
}

fn no_progress(_: Progress) {}

fn kinds(outcomes: &[OpOutcome]) -> Vec<&'static str> {
    outcomes
        .iter()
        .map(|outcome| match &outcome.result {
            Ok(OpSuccess::Already { .. }) => "already",
            Ok(OpSuccess::Completed { .. }) => "completed",
            Ok(OpSuccess::Submitted { .. }) => "submitted",
            Err(HyperVError::NotFound { .. }) => "not-found",
            Err(HyperVError::Ambiguous { .. }) => "ambiguous",
            Err(HyperVError::InvalidState { .. }) => "invalid-state",
            Err(HyperVError::Timeout { .. }) => "timeout",
            Err(other) => panic!("unexpected error for '{}': {other}", outcome.target),
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_name_that_matches_two_vms_is_reported_as_ambiguous() {
    // Acceptance test 1: the two `dup` VMs must not be guessed between.
    let ops = operations(fixture()).await;
    let outcomes = ops
        .run_vm(
            VmOp::Start,
            &specs(&["dup"]),
            WaitPolicy::Wait(None),
            &mut no_progress,
        )
        .await;
    assert_eq!(first(&outcomes).target, "dup");
    match failure(first(&outcomes)) {
        HyperVError::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
        other => panic!("expected an ambiguous target, got {other:?}"),
    }
    assert_eq!(kinds(&outcomes), ["ambiguous"]);
}

#[tokio::test(start_paused = true)]
async fn a_goal_that_is_already_met_reports_success_without_a_request() {
    // Acceptance test 5: already running / already stopped is a success.
    let ops = operations(fixture()).await;
    let outcomes = ops
        .run_vm(
            VmOp::Start,
            &specs(&["web-01"]),
            WaitPolicy::Wait(Some(Duration::from_secs(60))),
            &mut no_progress,
        )
        .await;
    match success(first(&outcomes)) {
        OpSuccess::Already { state } => assert_eq!(*state, VmState::Running),
        other => panic!("expected 'already', got {other:?}"),
    }

    let stopped = ops
        .run_vm(
            VmOp::Stop,
            &specs(&["web-02"]),
            WaitPolicy::Wait(Some(Duration::from_secs(60))),
            &mut no_progress,
        )
        .await;
    match success(first(&stopped)) {
        OpSuccess::Already { state } => assert_eq!(*state, VmState::Off),
        other => panic!("expected 'already', got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn an_operation_the_state_cannot_do_is_invalid() {
    // Spec 2.9: `pause` on a VM that is off is a failure, not a no-op.
    let ops = operations(fixture()).await;
    let outcomes = ops
        .run_vm(
            VmOp::Pause,
            &specs(&["web-02"]),
            WaitPolicy::Wait(None),
            &mut no_progress,
        )
        .await;
    assert_eq!(kinds(&outcomes), ["invalid-state"]);
    match failure(first(&outcomes)) {
        HyperVError::InvalidState { state, .. } => assert_eq!(*state, VmState::Off),
        other => panic!("expected an invalid state, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_timeout_stops_waiting_but_leaves_the_job_running() {
    // Acceptance test 6: waiting gives up, the job is not cancelled.
    let mut backend = fixture();
    // Jobs stay running for their first ten polls, so the wait times out while
    // they are still going and the test can then watch the same job finish,
    // without depending on any clock.
    backend.hold_jobs_for_polls(10);
    let ops = operations(backend).await;

    let outcomes = ops
        .run_vm(
            VmOp::Start,
            &specs(&["web-02"]),
            WaitPolicy::Wait(Some(Duration::from_secs(1))),
            &mut no_progress,
        )
        .await;
    match failure(first(&outcomes)) {
        HyperVError::Timeout { .. } => {}
        other => panic!("expected a timeout, got {other:?}"),
    }

    // The job the timeout abandoned is still running...
    let running = ok(ops.handle().job_status(JobRef::from("mock-job-1")).await);
    assert!(matches!(running, JobStatus::Running(_)), "got {running:?}");

    // ...and finishes on its own, because a timeout never cancels anything.
    let mut finished = running;
    for _ in 0..20 {
        finished = ok(ops.handle().job_status(JobRef::from("mock-job-1")).await);
        if finished == JobStatus::Succeeded {
            break;
        }
    }
    assert_eq!(finished, JobStatus::Succeeded);

    let again = ops
        .run_vm(
            VmOp::Start,
            &specs(&["web-02"]),
            WaitPolicy::Wait(None),
            &mut no_progress,
        )
        .await;
    match success(first(&again)) {
        OpSuccess::Already { state } => assert_eq!(*state, VmState::Running),
        other => panic!("expected the job to have started the VM, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn one_failure_does_not_stop_the_remaining_targets() {
    // Acceptance test 7: every target runs; the outcomes stay in order.
    let ops = operations(fixture()).await;
    let outcomes = ops
        .run_vm(
            VmOp::Stop,
            &specs(&["web-01", "ghost-vm", "web-02"]),
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    assert_eq!(outcomes.len(), 3);
    assert_eq!(kinds(&outcomes), ["completed", "not-found", "already"]);
}

#[tokio::test(start_paused = true)]
async fn restart_requests_a_guest_reboot_and_completes_when_the_uptime_resets() {
    let ops = operations(fixture()).await;
    let mut phases: Vec<Phase> = Vec::new();
    let mut record = |progress: Progress| phases.push(progress.phase);

    let outcomes = ops
        .run_vm(
            VmOp::Restart,
            &specs(&["web-01"]),
            WaitPolicy::Wait(Some(Duration::from_secs(60))),
            &mut record,
        )
        .await;

    match success(first(&outcomes)) {
        OpSuccess::Completed { state, .. } => assert_eq!(*state, VmState::Running),
        other => panic!("expected a completed restart, got {other:?}"),
    }
    assert_eq!(phases.first(), Some(&Phase::Resolving));
    assert_eq!(phases.last(), Some(&Phase::Finished));
    let requests = phases
        .iter()
        .filter(|phase| **phase == Phase::Requesting)
        .count();
    assert_eq!(
        requests, 1,
        "restart must send exactly one InitiateReboot request"
    );
    // The mock reset the uptime after the reboot settled.
    let vm = ok(ops
        .handle()
        .get_vm(ok(ops.resolve_vm("web-01").await))
        .await);
    assert_eq!(vm.summary.uptime, Some(Duration::ZERO));
}

#[tokio::test(start_paused = true)]
async fn a_forced_restart_is_a_single_reset() {
    let ops = operations(fixture()).await;
    let mut phases: Vec<Phase> = Vec::new();
    let mut record = |progress: Progress| phases.push(progress.phase);

    let outcomes = ops
        .run_vm(
            VmOp::RestartForce,
            &specs(&["web-01"]),
            WaitPolicy::Wait(Some(Duration::from_secs(60))),
            &mut record,
        )
        .await;

    match success(first(&outcomes)) {
        OpSuccess::Completed { state, .. } => assert_eq!(*state, VmState::Running),
        other => panic!("expected a completed reset, got {other:?}"),
    }
    let requests = phases
        .iter()
        .filter(|phase| **phase == Phase::Requesting)
        .count();
    assert_eq!(requests, 1, "--force must reset in one request");
}

#[tokio::test(start_paused = true)]
async fn no_wait_returns_right_after_the_backend_accepts() {
    let ops = operations(fixture()).await;
    let mut phases: Vec<Phase> = Vec::new();
    let mut record = |progress: Progress| phases.push(progress.phase);

    let outcomes = ops
        .run_vm(
            VmOp::Start,
            &specs(&["web-02"]),
            WaitPolicy::NoWait,
            &mut record,
        )
        .await;

    match success(first(&outcomes)) {
        OpSuccess::Submitted {
            pending: Pending::Job(_),
        } => {}
        other => panic!("expected a submitted job, got {other:?}"),
    }
    assert!(
        !phases.contains(&Phase::Waiting),
        "--no-wait must not wait: {phases:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_guest_not_ready_shutdown_fails_immediately_instead_of_waiting() {
    // Anomaly 16: the job fails at once; waiting must not turn this into a
    // timeout, and the VM stays Running (M0 item 5).
    let mut backend = fixture();
    backend.inject_guest_not_ready();
    let ops = operations(backend).await;

    let outcomes = ops
        .run_vm(
            VmOp::Stop,
            &specs(&["web-01"]),
            WaitPolicy::Wait(Some(Duration::from_secs(60))),
            &mut no_progress,
        )
        .await;
    match failure(first(&outcomes)) {
        HyperVError::JobFailed { code, reason, .. } => {
            assert_eq!(*code, 0x800710DF);
            assert_eq!(*reason, JobFailReason::GuestNotReady);
        }
        other => panic!("expected a job failure, got {other:?}"),
    }

    let vm = ok(ops
        .handle()
        .get_vm(ok(ops.resolve_vm("web-01").await))
        .await);
    assert_eq!(vm.summary.state, VmState::Running);
}

#[tokio::test(start_paused = true)]
async fn a_restart_of_a_stopped_vm_is_invalid_and_sends_nothing() {
    // Anomaly 17: the table allows restart only from Running.
    let ops = operations(fixture()).await;
    let mut phases: Vec<Phase> = Vec::new();
    let mut record = |progress: Progress| phases.push(progress.phase);

    let outcomes = ops
        .run_vm(
            VmOp::Restart,
            &specs(&["web-02"]),
            WaitPolicy::Wait(Some(Duration::from_secs(60))),
            &mut record,
        )
        .await;
    match failure(first(&outcomes)) {
        HyperVError::InvalidState {
            state, operation, ..
        } => {
            assert_eq!(*state, VmState::Off);
            assert_eq!(operation, "restart");
        }
        other => panic!("expected an invalid state, got {other:?}"),
    }
    assert!(
        !phases.contains(&Phase::Requesting),
        "nothing may be requested: {phases:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn no_wait_on_a_transitional_state_reports_busy() {
    // Anomaly 18: a request accepted with `--no-wait` leaves the VM in a
    // transitional state, and the very next `--no-wait` operation is an
    // `InvalidState` naming that state (spec 2.9, wait cell).
    let mut backend = MockBackend::with_fixture();
    backend.set_tick_mode(TickMode::Manual);
    let ops = operations(backend).await;

    let paused = ops
        .run_vm(
            VmOp::Pause,
            &specs(&["web-01"]),
            WaitPolicy::NoWait,
            &mut no_progress,
        )
        .await;
    assert!(
        matches!(
            success(first(&paused)),
            OpSuccess::Submitted {
                pending: Pending::Job(_)
            }
        ),
        "expected the pause to be accepted, got {:?}",
        first(&paused).result
    );

    let started = ops
        .run_vm(
            VmOp::Start,
            &specs(&["web-01"]),
            WaitPolicy::NoWait,
            &mut no_progress,
        )
        .await;
    match failure(first(&started)) {
        HyperVError::InvalidState { state, .. } => assert_eq!(*state, VmState::Busy),
        other => panic!("expected an invalid state, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_guid_is_used_verbatim_and_never_ambiguous() {
    let ops = operations(fixture()).await;
    let id = ok(ops.resolve_vm("00000003-0000-0000-0000-000000000000").await);
    let vm = ok(ops.handle().get_vm(id).await);
    assert_eq!(vm.summary.name, "db-01");
    assert_eq!(vm.summary.state, VmState::Saved);
}

#[tokio::test(start_paused = true)]
async fn checkpoints_are_created_renamed_and_removed() {
    let ops = operations(fixture()).await;
    let vm = ok(ops.resolve_vm("web-02").await);

    // Create without a name; the operation layer renames it afterwards.
    let outcome = ops
        .create_checkpoint(
            "web-02",
            Some("before-work"),
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    assert!(matches!(outcome.result, Ok(OpSuccess::Completed { .. })));
    let created = match outcome.created {
        Some(pair) => pair,
        None => panic!("create_checkpoint must report the new checkpoint"),
    };
    assert_eq!(created.1, "before-work");

    let listed = ok(ops.handle().list_checkpoints(vm).await);
    let name = listed.first().map(|checkpoint| checkpoint.name.clone());
    assert_eq!(name, Some("before-work".to_string()));

    // Rename an existing checkpoint.
    let renamed = ops
        .rename_checkpoint(
            "web-02",
            "before-work",
            "renamed",
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await
        .result;
    assert!(matches!(renamed, Ok(OpSuccess::Completed { .. })));
    let listed = ok(ops.handle().list_checkpoints(vm).await);
    assert_eq!(
        listed.first().map(|checkpoint| checkpoint.name.as_str()),
        Some("renamed")
    );

    let removed = ops
        .remove_checkpoint(
            "web-02",
            "renamed",
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await
        .result;
    assert!(matches!(removed, Ok(OpSuccess::Completed { .. })));
    assert!(ok(ops.handle().list_checkpoints(vm).await).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_rename_still_reports_the_created_checkpoint() {
    // Anomaly 20: the checkpoint exists but could not be named, so the
    // operation layer must report its id and original name for cleanup
    // (design 3.3 step 4).
    let mut backend = fixture();
    backend.inject_rename_failure(HyperVError::backend(RawError::new("rename refused")));
    let ops = operations(backend).await;
    let vm = ok(ops.resolve_vm("web-02").await);

    let outcome = ops
        .create_checkpoint(
            "web-02",
            Some("wanted"),
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    let (id, name) = match outcome.created {
        Some(pair) => pair,
        None => panic!("the created checkpoint must still be reported"),
    };
    assert_eq!(name, "Checkpoint 1");
    assert!(outcome.result.is_err());

    let listed = ok(ops.handle().list_checkpoints(vm).await);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].name, "Checkpoint 1");
}

#[tokio::test(start_paused = true)]
async fn a_create_that_yields_no_single_checkpoint_is_refused() {
    // Anomaly 20: when the id difference is not exactly one, the operation
    // layer must not guess which checkpoint was created (design 3.3 step 3).
    let mut backend = fixture();
    backend.create_checkpoints_per_call(2);
    let ops = operations(backend).await;

    let outcome = ops
        .create_checkpoint(
            "web-02",
            Some("wanted"),
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    assert!(outcome.created.is_none());
    match outcome.result {
        Err(HyperVError::Backend { .. }) => {}
        Err(other) => panic!("expected a backend error, got {other:?}"),
        Ok(success) => panic!("expected a failure, got {success:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn restoring_a_running_vm_is_rejected_before_anything_starts() {
    // Spec 2.9: restore is `Reject` while Running, and the availability table
    // refuses it here before the request is sent (design 3.3).
    let ops = operations(fixture()).await;
    let outcome = ops
        .restore_checkpoint(
            "web-01",
            "before-upgrade",
            WaitPolicy::Wait(None),
            &mut no_progress,
        )
        .await;
    match outcome.result {
        Err(HyperVError::InvalidState {
            state, operation, ..
        }) => {
            assert_eq!(state, VmState::Running);
            // The operation layer's own wording, not the backend's method
            // name: this error came from the table, not from the defense.
            assert_eq!(operation, "checkpoint restore");
        }
        Err(other) => panic!("expected an invalid state, got {other}"),
        Ok(success) => panic!("expected a failure, got {success:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_checkpoint_restores_when_the_table_allows_it() {
    let ops = operations(fixture()).await;
    // web-02 is Off, where restore is `Ok`; it needs a checkpoint first.
    let created = ops
        .create_checkpoint(
            "web-02",
            None,
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    let (id, _) = match created.created {
        Some(pair) => pair,
        None => panic!("create_checkpoint must report the new checkpoint"),
    };
    let outcome = ops
        .restore_checkpoint(
            "web-02",
            &id.to_string(),
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    assert!(
        matches!(outcome.result, Ok(OpSuccess::Completed { .. })),
        "restore from Off must run, got {:?}",
        outcome.result
    );
}

#[tokio::test(start_paused = true)]
async fn checkpoint_create_is_refused_in_a_state_the_table_rejects() {
    // `ghost` is Unknown(42): spec 2.9 says ✗, and nothing may be created.
    let ops = operations(fixture()).await;
    let outcome = ops
        .create_checkpoint(
            "ghost",
            None,
            WaitPolicy::Wait(Some(Duration::from_secs(30))),
            &mut no_progress,
        )
        .await;
    match outcome.result {
        Err(HyperVError::InvalidState {
            state, operation, ..
        }) => {
            assert_eq!(state, VmState::Unknown(42));
            assert_eq!(operation, "checkpoint create");
        }
        Err(other) => panic!("expected an invalid state, got {other}"),
        Ok(success) => panic!("expected a failure, got {success:?}"),
    }
    let vm = ok(ops.resolve_vm("ghost").await);
    assert!(ok(ops.handle().list_checkpoints(vm).await).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_switch_resolves_by_name_or_guid_but_never_by_guesswork() {
    let ops = operations(fixture()).await;
    let by_name = ok(ops.resolve_switch("test-priv").await);
    let by_guid = ok(ops.resolve_switch(&by_name.to_string()).await);
    assert_eq!(by_name, by_guid);

    match ops.resolve_switch("no-such-switch").await {
        Err(HyperVError::NotFound {
            kind, specified, ..
        }) => {
            assert_eq!(kind, TargetKind::Switch);
            assert_eq!(specified, "no-such-switch");
        }
        other => panic!("expected a not-found switch, got {other:?}"),
    }

    match ops.resolve_switch("dup-switch").await {
        Err(HyperVError::Ambiguous { candidates, .. }) => assert_eq!(candidates.len(), 2),
        other => panic!("expected an ambiguous switch, got {other:?}"),
    }
}
