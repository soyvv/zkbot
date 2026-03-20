//! Integration tests for the three-queue topology: priority control channel,
//! grace queue for reliable data events, and lossy tick path.
//!
//! These tests exercise the channel topology without any NATS/TradingClient
//! dependencies — just in-process mpsc channels mirroring the runtime setup.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;

use zk_engine_rs::{ControlCommand, EngineEvent, EventEnvelope};

fn envelope(event: EngineEvent) -> EventEnvelope {
    EventEnvelope::now(event)
}

/// Spawn a priority forwarder identical to the one in runtime.rs.
/// Control events are drained before grace events via `biased select!`.
fn spawn_priority_forwarder(
    mut control_rx: mpsc::Receiver<EventEnvelope>,
    mut grace_rx: mpsc::Receiver<EventEnvelope>,
    engine_tx: mpsc::Sender<EventEnvelope>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let envelope = tokio::select! {
                biased;
                Some(env) = control_rx.recv() => env,
                Some(env) = grace_rx.recv() => env,
                else => break,
            };
            if engine_tx.send(envelope).await.is_err() {
                break;
            }
        }
    })
}

/// Extract the Timer(ms) value from an envelope, panicking on other variants.
fn timer_ms(env: &EventEnvelope) -> i64 {
    match &env.event {
        EngineEvent::Timer(ms) => *ms,
        other => panic!("expected Timer, got {:?}", other),
    }
}

fn is_control(env: &EventEnvelope) -> bool {
    matches!(&env.event, EngineEvent::Control(_))
}

/// Helper to receive with a timeout so tests don't hang on failure.
async fn recv_timeout(rx: &mut mpsc::Receiver<EventEnvelope>) -> EventEnvelope {
    timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("recv timed out")
        .expect("channel closed unexpectedly")
}

// ─── Grace queue ordering ────────────────────────────────────────────

#[tokio::test]
async fn grace_forwarder_preserves_event_order() {
    let (engine_tx, mut engine_rx) = mpsc::channel(64);
    let (control_tx, control_rx) = mpsc::channel(32);
    let (grace_tx, grace_rx) = mpsc::channel(64);

    let _fwd = spawn_priority_forwarder(control_rx, grace_rx, engine_tx);

    // Send 10 reliable events through grace queue.
    for i in 0..10 {
        grace_tx
            .send(envelope(EngineEvent::Timer(i)))
            .await
            .unwrap();
    }
    drop(grace_tx);
    drop(control_tx);

    // Receive and verify order.
    for i in 0..10 {
        let env = recv_timeout(&mut engine_rx).await;
        assert_eq!(timer_ms(&env), i, "event {i} out of order");
    }
}

#[tokio::test]
async fn ticks_and_reliable_events_both_reach_engine() {
    let (engine_tx, mut engine_rx) = mpsc::channel(64);
    let (control_tx, control_rx) = mpsc::channel(32);
    let (grace_tx, grace_rx) = mpsc::channel(64);

    let _fwd = spawn_priority_forwarder(control_rx, grace_rx, engine_tx.clone());

    // Tick goes direct to engine channel.
    engine_tx
        .send(envelope(EngineEvent::Tick(Default::default())))
        .await
        .unwrap();

    // Bar goes through grace queue.
    grace_tx
        .send(envelope(EngineEvent::Bar(Default::default())))
        .await
        .unwrap();

    // Control goes through control queue.
    control_tx
        .send(envelope(EngineEvent::Control(ControlCommand::Pause {
            reason: "test".into(),
        })))
        .await
        .unwrap();

    // All three should arrive.
    let mut received = Vec::new();
    for _ in 0..3 {
        received.push(recv_timeout(&mut engine_rx).await);
    }

    assert!(received
        .iter()
        .any(|e| matches!(&e.event, EngineEvent::Tick(_))));
    assert!(received
        .iter()
        .any(|e| matches!(&e.event, EngineEvent::Bar(_))));
    assert!(received
        .iter()
        .any(|e| matches!(&e.event, EngineEvent::Control(_))));
}

// ─── Priority: control before grace ──────────────────────────────────

#[tokio::test]
async fn control_events_forwarded_before_grace_events() {
    // Use engine capacity of 1 so forwarder processes one event at a time.
    let (engine_tx, mut engine_rx) = mpsc::channel(1);
    let (control_tx, control_rx) = mpsc::channel(32);
    let (grace_tx, grace_rx) = mpsc::channel(64);

    // Pre-fill both queues BEFORE starting forwarder so all events are pending.
    grace_tx.try_send(envelope(EngineEvent::Timer(1))).unwrap();
    grace_tx.try_send(envelope(EngineEvent::Timer(2))).unwrap();
    grace_tx.try_send(envelope(EngineEvent::Timer(3))).unwrap();

    control_tx
        .try_send(envelope(EngineEvent::Control(ControlCommand::Pause {
            reason: "priority-test".into(),
        })))
        .unwrap();

    // Drop senders so forwarder can exit after draining.
    drop(control_tx);
    drop(grace_tx);

    // Now start the forwarder — it will see both queues with pending items.
    let fwd = spawn_priority_forwarder(control_rx, grace_rx, engine_tx);

    // Collect all forwarded events.
    let mut events = Vec::new();
    for _ in 0..4 {
        events.push(recv_timeout(&mut engine_rx).await);
    }

    // Wait for forwarder to finish.
    timeout(Duration::from_secs(2), fwd)
        .await
        .expect("forwarder did not exit")
        .expect("forwarder panicked");

    // The control event should come first (biased select).
    assert!(
        is_control(&events[0]),
        "expected control event first, got {:?}",
        events[0].event,
    );

    // Grace events should follow in order.
    let timer_values: Vec<i64> = events[1..].iter().map(|e| timer_ms(e)).collect();
    assert_eq!(timer_values, vec![1, 2, 3]);
}

// ─── Shutdown drain ──────────────────────────────────────────────────

#[tokio::test]
async fn forwarder_drains_before_exit() {
    let (engine_tx, mut engine_rx) = mpsc::channel(64);
    let (control_tx, control_rx) = mpsc::channel(32);
    let (grace_tx, grace_rx) = mpsc::channel(64);

    let fwd = spawn_priority_forwarder(control_rx, grace_rx, engine_tx);

    // Enqueue events then close senders (simulating shutdown).
    for i in 0..5 {
        grace_tx
            .send(envelope(EngineEvent::Timer(i)))
            .await
            .unwrap();
    }
    drop(grace_tx);
    drop(control_tx);

    // Forwarder should drain all events and then exit.
    timeout(Duration::from_secs(2), fwd)
        .await
        .expect("forwarder did not exit in time")
        .expect("forwarder panicked");

    // All 5 events should have been forwarded.
    for i in 0..5 {
        let env = engine_rx
            .try_recv()
            .expect("expected event in engine channel");
        assert_eq!(timer_ms(&env), i);
    }
}

#[tokio::test]
async fn forwarder_exits_when_engine_channel_closes() {
    let (engine_tx, engine_rx) = mpsc::channel(4);
    let (control_tx, control_rx) = mpsc::channel(32);
    let (grace_tx, grace_rx) = mpsc::channel(64);

    let fwd = spawn_priority_forwarder(control_rx, grace_rx, engine_tx);

    // Drop engine receiver — engine_tx.send() will fail in forwarder.
    drop(engine_rx);

    grace_tx
        .send(envelope(EngineEvent::Timer(1)))
        .await
        .unwrap();
    drop(grace_tx);
    drop(control_tx);

    // Forwarder should exit promptly after send failure.
    timeout(Duration::from_secs(2), fwd)
        .await
        .expect("forwarder did not exit in time")
        .expect("forwarder panicked");
}

// ─── Full shutdown simulation ────────────────────────────────────────

#[tokio::test]
async fn full_shutdown_sequence_does_not_hang() {
    let (engine_tx, mut engine_rx) = mpsc::channel(16);
    let (control_tx, control_rx) = mpsc::channel(32);
    let (grace_tx, grace_rx) = mpsc::channel(16);

    let engine_tx_fwd = engine_tx.clone();
    let priority_forwarder = spawn_priority_forwarder(control_rx, grace_rx, engine_tx_fwd);

    // Simulate subscription tasks holding grace_tx clones.
    let sub_grace = grace_tx.clone();
    let sub_handle = tokio::spawn(async move {
        let _hold = sub_grace;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    // Simulate gRPC task holding control_tx clone.
    let grpc_ctrl = control_tx.clone();
    let grpc_handle = tokio::spawn(async move {
        let _hold = grpc_ctrl;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    // Simulate timer holding engine_tx clone.
    let timer_tx = engine_tx.clone();
    let timer_handle = tokio::spawn(async move {
        let _hold = timer_tx;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    // Enqueue some events.
    grace_tx
        .send(envelope(EngineEvent::Timer(1)))
        .await
        .unwrap();
    grace_tx
        .send(envelope(EngineEvent::Timer(2)))
        .await
        .unwrap();

    // Send stop directly to engine (bypass both queues).
    engine_tx
        .send(envelope(EngineEvent::Control(ControlCommand::Stop {
            reason: "test shutdown".into(),
        })))
        .await
        .unwrap();

    // Shutdown sequence (mirrors runtime.rs):
    grpc_handle.abort();
    sub_handle.abort();
    timer_handle.abort();

    drop(control_tx);
    drop(grace_tx);
    drop(engine_tx);

    let fwd_result = timeout(Duration::from_secs(2), priority_forwarder).await;
    assert!(
        fwd_result.is_ok(),
        "priority forwarder hung during shutdown"
    );

    // Drain engine_rx — all events must be present.
    let mut events = Vec::new();
    while let Ok(env) = engine_rx.try_recv() {
        events.push(env);
    }

    assert_eq!(events.len(), 3, "expected 3 events, got {}", events.len());

    let has_stop = events
        .iter()
        .any(|e| matches!(&e.event, EngineEvent::Control(ControlCommand::Stop { .. })));
    assert!(has_stop, "Stop command missing from engine channel");

    let timer_values: Vec<i64> = events
        .iter()
        .filter_map(|e| match &e.event {
            EngineEvent::Timer(ms) => Some(*ms),
            _ => None,
        })
        .collect();
    assert_eq!(timer_values.len(), 2, "expected 2 timer events");
    assert_eq!(timer_values, vec![1, 2]);
}

// ─── Overflow ────────────────────────────────────────────────────────

#[tokio::test]
async fn grace_queue_rejects_when_full() {
    let (grace_tx, _grace_rx) = mpsc::channel(2);

    grace_tx.try_send(envelope(EngineEvent::Timer(1))).unwrap();
    grace_tx.try_send(envelope(EngineEvent::Timer(2))).unwrap();

    let result = grace_tx.try_send(envelope(EngineEvent::Timer(3)));
    assert!(
        matches!(result, Err(mpsc::error::TrySendError::Full(_))),
        "expected grace queue to be full"
    );
}

// ─── Control timeout ─────────────────────────────────────────────────

#[tokio::test]
async fn control_try_send_timeout_on_full_queue() {
    // Capacity 1: first send fills, second must wait.
    let (control_tx, _control_rx) = mpsc::channel(1);

    control_tx
        .try_send(envelope(EngineEvent::Control(ControlCommand::Pause {
            reason: "fill".into(),
        })))
        .unwrap();

    // Second try_send should fail (full).
    let env = envelope(EngineEvent::Control(ControlCommand::Resume {
        reason: "overflow".into(),
    }));
    match control_tx.try_send(env) {
        Err(mpsc::error::TrySendError::Full(env)) => {
            // Slow path: timeout should fire since nobody is draining.
            let result =
                tokio::time::timeout(Duration::from_millis(50), control_tx.send(env)).await;
            assert!(result.is_err(), "expected timeout, got Ok");
        }
        other => panic!("expected Full, got {:?}", other.map(|_| ())),
    }
}
