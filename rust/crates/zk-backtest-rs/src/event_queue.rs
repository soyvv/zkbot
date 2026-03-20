use std::cmp::Ordering;
use std::collections::BinaryHeap;

use zk_proto_rs::zk::{
    oms::v1::{BalanceUpdateEvent, OrderUpdateEvent, PositionUpdateEvent},
    rtmd::v1::{Kline, RealtimeSignal, TickData},
};

use zk_strategy_sdk_rs::models::TimerEvent;

/// All event types that flow through the backtester's replay queue.
/// Type priority is used for tie-breaking when timestamps are equal —
/// higher priority number = processed first (matches Python `_type_ord`).
pub enum BtEventKind {
    Tick(TickData),                      // priority 10
    Signal(RealtimeSignal),              // priority 9
    Bar(Kline),                          // priority 8
    BalanceUpdate(BalanceUpdateEvent),   // priority 7 (asset inventory)
    PositionUpdate(PositionUpdateEvent), // priority 7 (derivatives exposure)
    OrderUpdate(OrderUpdateEvent),       // priority 6
    Timer(TimerEvent),                   // priority 5
}

impl BtEventKind {
    pub(crate) fn priority(&self) -> u8 {
        match self {
            BtEventKind::Tick(_) => 10,
            BtEventKind::Signal(_) => 9,
            BtEventKind::Bar(_) => 8,
            BtEventKind::BalanceUpdate(_) => 7,
            BtEventKind::PositionUpdate(_) => 7,
            BtEventKind::OrderUpdate(_) => 6,
            BtEventKind::Timer(_) => 5,
        }
    }
}

/// An event with its scheduled delivery timestamp.
pub struct BtEvent {
    pub ts_ms: i64,
    pub kind: BtEventKind,
}

// ---------------------------------------------------------------------------
// OrdEvent — wrapper for BinaryHeap (dynamic events)
// ---------------------------------------------------------------------------

// Min-heap by (ts_ms ASC, priority DESC) via max-heap with inverted comparator.
struct OrdEvent(BtEvent);

impl PartialEq for OrdEvent {
    fn eq(&self, other: &Self) -> bool {
        self.0.ts_ms == other.0.ts_ms && self.0.kind.priority() == other.0.kind.priority()
    }
}
impl Eq for OrdEvent {}

impl PartialOrd for OrdEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lower ts first, then higher priority first.
        match other.0.ts_ms.cmp(&self.0.ts_ms) {
            Ordering::Equal => self.0.kind.priority().cmp(&other.0.kind.priority()),
            ord => ord,
        }
    }
}

// ---------------------------------------------------------------------------
// SortedStream — pre-sorted static event source (klines, ticks, signals)
// ---------------------------------------------------------------------------

/// A pre-sorted sequence of static events, consumed lazily by `EventQueue::pop`.
///
/// Holds events in sorted order (by timestamp); only the current head is ever
/// compared against the dynamic heap. No heap insertions occur for static events
/// — they are visited in O(1) per event instead of O(log n).
///
/// Mirrors Python `ReorderQueue`'s per-type DataFrame + index cursor pattern.
pub struct SortedStream {
    events: Vec<(i64, BtEventKind)>,
    idx: usize,
}

impl SortedStream {
    fn new(mut events: Vec<(i64, BtEventKind)>) -> Self {
        // Guarantee sort order regardless of caller — O(n log n) once at load time.
        events.sort_unstable_by_key(|(ts, _)| *ts);
        Self { events, idx: 0 }
    }

    /// Peek at the current head: returns `(ts_ms, priority)` without consuming.
    fn peek(&self) -> Option<(i64, u8)> {
        self.events
            .get(self.idx)
            .map(|(ts, kind)| (*ts, kind.priority()))
    }

    /// Consume and return the current head.
    fn advance(&mut self) -> Option<(i64, BtEventKind)> {
        if self.idx < self.events.len() {
            // SAFETY: we just checked bounds; the move is safe because we advance idx.
            // To avoid cloning, use swap-and-replace with a dummy.
            let ts = self.events[self.idx].0;
            // Replace the slot with a dummy Bar so we can move the kind out.
            let kind = std::mem::replace(
                &mut self.events[self.idx].1,
                BtEventKind::Bar(Kline::default()),
            );
            self.idx += 1;
            Some((ts, kind))
        } else {
            None
        }
    }

    fn is_empty(&self) -> bool {
        self.idx >= self.events.len()
    }

    fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.idx)
    }
}

// ---------------------------------------------------------------------------
// EventQueue — k-way merge of sorted streams + dynamic heap
// ---------------------------------------------------------------------------

/// Priority queue for backtester events.
///
/// Implements a **k-way merge** of:
/// - `streams`: pre-sorted static data (klines, ticks, signals) — O(1) peek/advance.
/// - `dynamic`: small `BinaryHeap` for events generated during simulation
///   (OMS order/position updates, timer fires) — stays tiny.
///
/// This mirrors the Python `ReorderQueue` design where each data type is held in
/// a separate DataFrame and only the *next* entry of each type lives in the
/// priority heap. The total heap size is O(k + dynamic) rather than O(n_events),
/// eliminating O(n log n) pre-load cost for long backtests.
pub struct EventQueue {
    streams: Vec<SortedStream>,
    dynamic: BinaryHeap<OrdEvent>,
}

impl EventQueue {
    pub fn new() -> Self {
        Self {
            streams: Vec::new(),
            dynamic: BinaryHeap::new(),
        }
    }

    /// Add a pre-sorted static event stream (e.g. all klines for a symbol).
    /// The vector is sorted at insertion time — callers need not pre-sort.
    pub fn add_stream(&mut self, events: Vec<(i64, BtEventKind)>) {
        if !events.is_empty() {
            self.streams.push(SortedStream::new(events));
        }
    }

    /// Push a single dynamic event (OMS output, timer fire) into the small heap.
    pub fn push(&mut self, ts_ms: i64, kind: BtEventKind) {
        self.dynamic.push(OrdEvent(BtEvent { ts_ms, kind }));
    }

    /// Pop and return the next event (minimum ts, maximum priority on tie).
    ///
    /// Performs a k-way merge: compares the head of each sorted stream against
    /// the dynamic heap's minimum, then pops the winner.
    pub fn pop(&mut self) -> Option<BtEvent> {
        // Candidate from dynamic heap: (ts_ms, priority)
        let dyn_peek = self
            .dynamic
            .peek()
            .map(|e| (e.0.ts_ms, e.0.kind.priority()));

        // Best candidate across all sorted streams: (stream_idx, ts_ms, priority)
        let stream_best: Option<(usize, i64, u8)> = self
            .streams
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.peek().map(|(ts, pri)| (i, ts, pri)))
            .min_by(|a, b| {
                // Lower ts wins; higher priority wins on tie.
                a.1.cmp(&b.1).then(b.2.cmp(&a.2))
            });

        match (dyn_peek, stream_best) {
            (None, None) => None,

            (Some(_), None) => self.dynamic.pop().map(|e| e.0),

            (None, Some((si, _, _))) => {
                let (ts, kind) = self.streams[si].advance().unwrap();
                Some(BtEvent { ts_ms: ts, kind })
            }

            (Some((dts, dpri)), Some((si, sts, spri))) => {
                // Dynamic wins on strictly lower ts, or equal ts with >= priority.
                let dyn_wins = match dts.cmp(&sts) {
                    Ordering::Less => true,
                    Ordering::Greater => false,
                    Ordering::Equal => dpri >= spri,
                };
                if dyn_wins {
                    self.dynamic.pop().map(|e| e.0)
                } else {
                    let (ts, kind) = self.streams[si].advance().unwrap();
                    Some(BtEvent { ts_ms: ts, kind })
                }
            }
        }
    }

    /// Peek at the timestamp of the next event without consuming it.
    pub fn peek_ts(&self) -> Option<i64> {
        let dyn_ts = self.dynamic.peek().map(|e| e.0.ts_ms);
        let stream_ts = self
            .streams
            .iter()
            .filter_map(|s| s.peek().map(|(ts, _)| ts))
            .min();
        match (dyn_ts, stream_ts) {
            (None, None) => None,
            (Some(d), None) => Some(d),
            (None, Some(s)) => Some(s),
            (Some(d), Some(s)) => Some(d.min(s)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dynamic.is_empty() && self.streams.iter().all(|s| s.is_empty())
    }

    /// Total events remaining (streams + dynamic heap).
    pub fn len(&self) -> usize {
        self.dynamic.len() + self.streams.iter().map(|s| s.remaining()).sum::<usize>()
    }
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}
