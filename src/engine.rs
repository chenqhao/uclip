//! Pure sans-I/O synchronization engine for uclip.
//!
//! This module contains the state machine that manages logical clocks,
//! resolves sync conflicts (Last-Writer-Wins), and suppresses echoes.

use crate::DeviceId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A hybrid logical clock timestamp combining physical wall time and a logical counter.
///
/// Implements [`Ord`] using field-declaration order:
/// 1. Compares `wall_ms` first (physical time in milliseconds).
/// 2. If milliseconds are identical, compares `counter` (logical tie-breaker).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct HybridTimestamp {
    pub wall_ms: u64,
    pub counter: u32,
}

impl HybridTimestamp {
    /// Create a new timestamp with the given physical time and logical counter.
    pub const fn new(wall_ms: u64, counter: u32) -> Self {
        Self { wall_ms, counter }
    }
}

/// A Hybrid Logical Clock (HLC).
///
/// Combines physical wall-clock time with a Lamport logical counter.
/// Guarantees that timestamps are strictly monotonic, tracks real time when available,
/// and never moves backwards even if the OS system clock jumps back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HybridLogicalClock {
    latest: HybridTimestamp,
}

impl HybridLogicalClock {
    /// Create a new HLC initialized to zero.
    pub const fn new() -> Self {
        Self {
            latest: HybridTimestamp::new(0, 0),
        }
    }

    /// Read the current timestamp without advancing the clock.
    pub fn get(&self) -> HybridTimestamp {
        self.latest
    }

    /// Advance the clock for a local event (e.g. copying new text).
    ///
    /// - If physical time `now_ms` has progressed past `latest.wall_ms`, adopts `now_ms` and resets counter to 0.
    /// - Otherwise (same millisecond or clock stepped backward), keeps `latest.wall_ms` and increments counter.
    pub fn tick(&mut self, now_ms: u64) -> HybridTimestamp {
        if now_ms > self.latest.wall_ms {
            self.latest = HybridTimestamp::new(now_ms, 0);
        } else {
            self.latest.counter += 1;
        }
        self.latest
    }

    /// Advance the clock upon witnessing a remote timestamp.
    ///
    /// - Rejects timestamps that exceed `now_ms + max_drift_ms` to prevent future clock poisoning.
    /// - Adopts the maximum of local physical time, latest wall time, and remote wall time.
    /// - Adjusts `counter` to guarantee that the new timestamp is strictly greater than both
    ///   the previous local timestamp and the received remote timestamp.
    pub fn witness(
        &mut self,
        remote: HybridTimestamp,
        now_ms: u64,
        max_drift_ms: u64,
    ) -> Result<HybridTimestamp, ClockDriftError> {
        if remote.wall_ms > now_ms.saturating_add(max_drift_ms) {
            return Err(ClockDriftError);
        }

        let w = now_ms.max(self.latest.wall_ms).max(remote.wall_ms);
        let c = if w == self.latest.wall_ms && w == remote.wall_ms {
            self.latest.counter.max(remote.counter) + 1
        } else if w == self.latest.wall_ms {
            self.latest.counter + 1
        } else if w == remote.wall_ms {
            remote.counter + 1
        } else {
            0
        };

        self.latest = HybridTimestamp::new(w, c);
        Ok(self.latest)
    }
}

/// Default maximum allowable clock drift for remote updates (60 seconds).
pub const DEFAULT_MAX_DRIFT_MS: u64 = 60_000;

/// Error emitted when a remote peer's clock exceeds allowable physical drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("remote timestamp exceeds maximum allowable drift")]
pub struct ClockDriftError;

/// Identifies an update in distributed logical time.
///
/// Implements [`Ord`] using Rust's field-declaration order:
/// 1. Higher `timestamp` wins (physical wall time first, counter tie-breaker second).
/// 2. If timestamps are tied, higher `origin` [`DeviceId`] wins (deterministic tie-breaker).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UpdateId {
    pub timestamp: HybridTimestamp,
    pub origin: DeviceId,
}

/// A monotonic Lamport logical clock.
///
/// Tracks the order of events in a distributed system without relying on wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LamportClock {
    counter: u64,
}

impl LamportClock {
    /// Create a new logical clock starting at zero.
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    /// Read the current logical timestamp without advancing the clock.
    pub fn get(&self) -> u64 {
        self.counter
    }

    /// Advance the clock for a local event (Rule 1).
    ///
    /// Increments the counter by 1 and returns the new value.
    pub fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Update the clock upon receiving a remote event (Rule 2).
    ///
    /// Sets the counter to `max(local, remote) + 1` and returns the new value.
    pub fn witness(&mut self, remote_time: u64) -> u64 {
        self.counter = self.counter.max(remote_time) + 1;
        self.counter
    }
}

use crate::ClipContent;
use crate::proto::Message;
use uuid::Uuid;

/// Actions emitted by the sync engine for the outer I/O layer to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Broadcast a new clip update to all connected peers over the network.
    Broadcast(Message),
    /// Apply new content to the local operating system clipboard.
    ApplyToClipboard(ClipContent),
}

/// The pure sans-I/O state machine that coordinates clipboard synchronization.
pub struct Engine {
    device_id: DeviceId,
    clock: HybridLogicalClock,
    current_hash: Option<[u8; 32]>,
    current_update: Option<UpdateId>,
    suppression: HashMap<[u8; 32], u64>,
    max_item_bytes: usize,
    max_drift_ms: u64,
}

impl Engine {
    /// Create a new sync engine for the given local device ID.
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            clock: HybridLogicalClock::new(),
            current_hash: None,
            current_update: None,
            suppression: HashMap::new(),
            max_item_bytes: DEFAULT_MAX_ITEM_BYTES,
            max_drift_ms: DEFAULT_MAX_DRIFT_MS,
        }
    }

    /// Configure a custom maximum allowable clock drift (builder pattern).
    pub fn with_max_drift_ms(mut self, max: u64) -> Self {
        self.max_drift_ms = max;
        self
    }

    /// Configure a custom maximum clip size limit in bytes (builder pattern).
    pub fn with_max_item_bytes(mut self, max: usize) -> Self {
        self.max_item_bytes = max;
        self
    }

    /// Current hybrid logical clock timestamp.
    pub fn clock(&self) -> HybridTimestamp {
        self.clock.get()
    }

    /// Handle a local clipboard change event.
    ///
    /// If the content is new:
    /// - Advances the hybrid logical clock.
    /// - Records the new hash and update ID.
    /// - Emits [`Action::Broadcast`] containing a `Message::ClipUpdate`.
    ///
    /// If the content matches an unexpired echo or current hash, it is ignored.
    pub fn on_local_change(&mut self, content: ClipContent, now_ms: u64) -> Vec<Action> {
        // 0. Size check: ignore clips exceeding maximum item limit.
        if content.byte_size() > self.max_item_bytes {
            return Vec::new();
        }

        let hash = content.content_hash();

        // 1. Echo suppression: if we recently wrote this from a remote peer, ignore it.
        if self
            .suppression
            .get(&hash)
            .is_some_and(|&expires_at| now_ms < expires_at)
        {
            return Vec::new();
        }

        // 2. Deduplication: if identical to current content, ignore.
        if Some(hash) == self.current_hash {
            return Vec::new();
        }

        // 3. Advance hybrid logical clock and record new state.
        let timestamp = self.clock.tick(now_ms);
        self.current_hash = Some(hash);
        self.current_update = Some(UpdateId {
            timestamp,
            origin: self.device_id,
        });

        // 4. Emit Broadcast action.
        let update_msg = Message::ClipUpdate {
            update_id: Uuid::new_v4(),
            origin: self.device_id,
            timestamp,
            content,
        };

        vec![Action::Broadcast(update_msg)]
    }

    /// Handle an update message received from a remote peer.
    ///
    /// Resolves conflicts using Last-Writer-Wins (HLC timestamp, tie-broken by [`DeviceId`]).
    /// If the update is newer than the current state:
    /// - Advances the local HLC via witness.
    /// - Records the content hash in the echo suppression map (2 s TTL).
    /// - Emits [`Action::ApplyToClipboard`].
    ///
    /// Older or duplicate updates, clock-skewed packets, or messages from spoofed origins are ignored.
    pub fn on_remote_update(
        &mut self,
        from: DeviceId,
        message: Message,
        now_ms: u64,
    ) -> Vec<Action> {
        let Message::ClipUpdate {
            origin,
            timestamp,
            content,
            ..
        } = message
        else {
            return Vec::new();
        };

        // 1. Fail closed: ensure sender is not spoofing another device's origin.
        if origin != from {
            return Vec::new();
        }

        // 2. Size check: ignore remote updates exceeding maximum item limit.
        if content.byte_size() > self.max_item_bytes {
            return Vec::new();
        }

        // 3. Clock drift check & advance clock (HLC witness rule).
        // If remote timestamp exceeds max allowable drift, reject.
        if self
            .clock
            .witness(timestamp, now_ms, self.max_drift_ms)
            .is_err()
        {
            return Vec::new();
        }

        // 4. Last-Writer-Wins conflict resolution.
        let remote_id = UpdateId { timestamp, origin };
        if self.current_update.is_some_and(|curr| remote_id <= curr) {
            return Vec::new();
        }

        // 5. Remote update wins! Update state & suppress local echo.
        let hash = content.content_hash();
        self.current_hash = Some(hash);
        self.current_update = Some(remote_id);
        self.suppression.insert(hash, now_ms + SUPPRESSION_TTL_MS);

        vec![Action::ApplyToClipboard(content)]
    }

    /// Periodic heartbeat event.
    ///
    /// Prunes expired entries from the echo suppression map so memory is bounded.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        self.suppression
            .retain(|_, &mut expires_at| now_ms < expires_at);
        Vec::new()
    }

    /// Number of entries currently in the echo suppression map.
    pub fn suppression_count(&self) -> usize {
        self.suppression.len()
    }
}

/// Duration in milliseconds to suppress local echo after applying a remote clip (2 seconds).
pub const SUPPRESSION_TTL_MS: u64 = 2000;

/// Default maximum allowed clip size in bytes (10 MiB).
pub const DEFAULT_MAX_ITEM_BYTES: usize = 10_485_760;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_timestamp_orders_by_wall_then_counter() {
        let t1 = HybridTimestamp::new(100, 0);
        let t2 = HybridTimestamp::new(200, 0);
        let t3 = HybridTimestamp::new(100, 1);

        // Later wall-clock time wins (200 > 100), regardless of counter
        assert!(t2 > t1);
        assert!(t2 > t3);

        // Same wall-clock time (100 == 100) -> higher counter wins (1 > 0)
        assert!(t3 > t1);
    }

    #[test]
    fn hlc_ticks_locally_and_handles_clock_regression() {
        let mut hlc = HybridLogicalClock::new();

        // 1. First event at t=100 -> wall advances, counter is 0
        assert_eq!(hlc.tick(100), HybridTimestamp::new(100, 0));

        // 2. Second event in the exact same millisecond (t=100) -> counter increments to 1
        assert_eq!(hlc.tick(100), HybridTimestamp::new(100, 1));

        // 3. Third event where physical clock jumps backward (e.g. NTP adjustment to t=90)
        // -> HLC refuses to move backward; holds wall at 100 and increments counter to 2
        assert_eq!(hlc.tick(90), HybridTimestamp::new(100, 2));

        // 4. Fourth event when physical clock catches up and moves forward (t=150)
        // -> wall advances to 150, counter resets to 0
        assert_eq!(hlc.tick(150), HybridTimestamp::new(150, 0));
    }

    #[test]
    fn hlc_witness_advances_and_rejects_excessive_drift() {
        let mut hlc = HybridLogicalClock::new();

        // 1. Normal catch-up: local is at 100, remote arrives from 150 (within 60s limit).
        // Max wall becomes 150, counter becomes remote.counter + 1 = 1.
        let remote = HybridTimestamp::new(150, 0);
        let updated = hlc.witness(remote, 100, DEFAULT_MAX_DRIFT_MS).unwrap();
        assert_eq!(updated, HybridTimestamp::new(150, 1));

        // 2. Both at same millisecond (150): counter leaps past highest counter.
        let remote_tied = HybridTimestamp::new(150, 5);
        let updated_tied = hlc.witness(remote_tied, 120, DEFAULT_MAX_DRIFT_MS).unwrap();
        assert_eq!(updated_tied, HybridTimestamp::new(150, 6));

        // 3. Local physical clock catches up past 150 to 200: adopts 200 and resets counter to 0.
        let remote_older = HybridTimestamp::new(150, 10);
        let updated_local_ahead = hlc
            .witness(remote_older, 200, DEFAULT_MAX_DRIFT_MS)
            .unwrap();
        assert_eq!(updated_local_ahead, HybridTimestamp::new(200, 0));

        // 4. Clock drift attack / severe desync: remote claims time 70_000 ms ahead of local 200 ms.
        let rogue_remote = HybridTimestamp::new(200 + DEFAULT_MAX_DRIFT_MS + 1, 0);
        let err = hlc.witness(rogue_remote, 200, DEFAULT_MAX_DRIFT_MS);
        assert_eq!(err, Err(ClockDriftError));
    }

    #[test]
    fn clock_ticks_monotonically() {
        let mut clock = LamportClock::new();
        assert_eq!(clock.get(), 0);
        assert_eq!(clock.tick(), 1);
        assert_eq!(clock.tick(), 2);
        assert_eq!(clock.get(), 2);
    }

    #[test]
    fn witness_advances_clock_past_remote_time() {
        let mut clock = LamportClock::new();
        clock.tick(); // local = 1

        // Witness a future remote timestamp (e.g. 10)
        assert_eq!(clock.witness(10), 11);
        assert_eq!(clock.get(), 11);

        // Witness an older remote timestamp (e.g. 5) -> clock still moves forward!
        assert_eq!(clock.witness(5), 12);
        assert_eq!(clock.get(), 12);
    }

    #[test]
    fn update_id_orders_by_timestamp_then_origin() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();

        let id1 = UpdateId {
            timestamp: HybridTimestamp::new(100, 0),
            origin: dev_a,
        };
        let id2 = UpdateId {
            timestamp: HybridTimestamp::new(200, 0),
            origin: dev_a,
        };
        let id3 = UpdateId {
            timestamp: HybridTimestamp::new(100, 0),
            origin: dev_b,
        };

        // Higher physical timestamp wins (200 > 100)
        assert!(id2 > id1);

        // Same physical timestamp -> tie-broken by origin (dev_b > dev_a)
        assert!(id3 > id1);
    }

    #[test]
    fn on_local_change_emits_broadcast_for_new_content() {
        let dev_id: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let mut engine = Engine::new(dev_id);

        let content = ClipContent::Text {
            text: "hello world".to_string(),
        };
        let actions = engine.on_local_change(content.clone(), 1000);

        assert_eq!(actions.len(), 1);
        assert_eq!(engine.clock(), HybridTimestamp::new(1000, 0));

        match &actions[0] {
            Action::Broadcast(Message::ClipUpdate {
                origin,
                timestamp,
                content: msg_content,
                ..
            }) => {
                assert_eq!(*origin, dev_id);
                assert_eq!(*timestamp, HybridTimestamp::new(1000, 0));
                assert_eq!(msg_content, &content);
            }
            other => panic!("expected Action::Broadcast(ClipUpdate), got {:?}", other),
        }
    }

    #[test]
    fn on_local_change_deduplicates_identical_content() {
        let dev_id: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let mut engine = Engine::new(dev_id);

        let content = ClipContent::Text {
            text: "hello".to_string(),
        };
        let actions1 = engine.on_local_change(content.clone(), 1000);
        let actions2 = engine.on_local_change(content.clone(), 1050);

        assert_eq!(actions1.len(), 1);
        assert!(actions2.is_empty());
        assert_eq!(engine.clock(), HybridTimestamp::new(1000, 0));
    }

    #[test]
    fn remote_update_resolves_tie_using_higher_device_id() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let mut engine_a = Engine::new(dev_a);
        let mut engine_b = Engine::new(dev_b);

        let content_a = ClipContent::Text {
            text: "from A".to_string(),
        };
        let content_b = ClipContent::Text {
            text: "from B".to_string(),
        };

        // Both devices copy locally, each advancing to clock = 1
        let actions_a = engine_a.on_local_change(content_a, 1000);
        let actions_b = engine_b.on_local_change(content_b.clone(), 1000);

        let msg_b = match actions_b.into_iter().next().unwrap() {
            Action::Broadcast(msg) => msg,
            _ => unreachable!(),
        };
        let msg_a = match actions_a.into_iter().next().unwrap() {
            Action::Broadcast(msg) => msg,
            _ => unreachable!(),
        };

        // Device A receives B's update. Both are at lamport: 1, but dev_b > dev_a, so B wins!
        let actions_on_a = engine_a.on_remote_update(dev_b, msg_b, 1050);
        assert_eq!(actions_on_a, vec![Action::ApplyToClipboard(content_b)]);

        // Device B receives A's update. Both are at lamport: 1, but dev_a < dev_b, so A loses!
        let actions_on_b = engine_b.on_remote_update(dev_a, msg_a, 1050);
        assert!(actions_on_b.is_empty());
    }

    #[test]
    fn remote_update_sets_echo_suppression() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let mut engine_a = Engine::new(dev_a);
        let mut engine_b = Engine::new(dev_b);

        let content_b = ClipContent::Text {
            text: "from B".to_string(),
        };

        let actions_b = engine_b.on_local_change(content_b.clone(), 1000);

        let msg_b = match actions_b.into_iter().next().unwrap() {
            Action::Broadcast(msg) => msg,
            _ => unreachable!(),
        };

        let actions = engine_a.on_remote_update(dev_b, msg_b, 1000);
        assert_eq!(actions, vec![Action::ApplyToClipboard(content_b.clone())]);

        let echo_actions = engine_a.on_local_change(content_b, 1500);
        assert!(echo_actions.is_empty());
    }

    #[test]
    fn tick_prunes_expired_suppression_entries() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let mut engine_a = Engine::new(dev_a);
        let mut engine_b = Engine::new(dev_b);

        let content_b = ClipContent::Text {
            text: "from B".to_string(),
        };

        let actions_b = engine_b.on_local_change(content_b.clone(), 1000);

        let msg_b = match actions_b.into_iter().next().unwrap() {
            Action::Broadcast(msg) => msg,
            _ => unreachable!(),
        };

        let actions = engine_a.on_remote_update(dev_b, msg_b, 1000);
        assert_eq!(actions, vec![Action::ApplyToClipboard(content_b.clone())]);
        assert_eq!(engine_a.suppression_count(), 1);

        engine_a.tick(2500);
        assert_eq!(engine_a.suppression_count(), 1);

        let intermediate = ClipContent::Text {
            text: "intermediate".to_string(),
        };

        engine_a.on_local_change(intermediate, 2600);

        engine_a.tick(3500);
        assert_eq!(engine_a.suppression_count(), 0);

        let local_actions = engine_a.on_local_change(content_b, 3600);

        assert_eq!(local_actions.len(), 1);
    }

    #[test]
    fn local_change_ignores_clips_exceeding_max_bytes() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let mut engine = Engine::new(dev_a).with_max_item_bytes(5);

        let content = ClipContent::Text {
            text: "hello world".to_string(),
        };

        let actions = engine.on_local_change(content, 1000);

        assert!(actions.is_empty());
        assert_eq!(engine.clock(), HybridTimestamp::default());
    }

    #[test]
    fn remote_update_rejects_spoofed_origin() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let dev_rouge: DeviceId = "33333333-3333-3333-3333-333333333333".parse().unwrap();

        let mut engine_a = Engine::new(dev_a);

        let spoofed_msg = Message::ClipUpdate {
            update_id: uuid::Uuid::new_v4(),
            origin: dev_rouge,
            timestamp: HybridTimestamp::new(1000, 0),
            content: ClipContent::Text {
                text: "imposter".to_string(),
            },
        };

        let actions = engine_a.on_remote_update(dev_b, spoofed_msg, 1000);

        assert!(actions.is_empty());
        assert_eq!(engine_a.clock(), HybridTimestamp::default());
    }

    #[test]
    fn remote_update_ignores_clips_exceeding_max_bytes() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();

        let mut engine_a = Engine::new(dev_a).with_max_item_bytes(5);

        let msg = Message::ClipUpdate {
            update_id: uuid::Uuid::new_v4(),
            origin: dev_b,
            timestamp: HybridTimestamp::new(1000, 0),
            content: ClipContent::Text {
                text: "oversized clip".to_string(),
            },
        };

        let actions = engine_a.on_remote_update(dev_b, msg, 1000);

        assert!(actions.is_empty());
        assert_eq!(engine_a.clock(), HybridTimestamp::default());
    }

    #[test]
    fn restarted_peer_new_copy_is_accepted() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let mut engine_a = Engine::new(dev_a);

        // A has been running a while: 5 distinct local copies push its clock to 5000 ms.
        for i in 1..=5 {
            let clip = ClipContent::Text {
                text: format!("clip {i}"),
            };
            engine_a.on_local_change(clip, i * 1000);
        }
        assert_eq!(engine_a.clock(), HybridTimestamp::new(5000, 0));

        // B's daemon restarts: a fresh Engine is exactly what a new process gets (clock = (0, 0)).
        let mut engine_b = Engine::new(dev_b);

        // Much later in real time, the user copies something brand new on B.
        let fresh = ClipContent::Text {
            text: "copied after restart".to_string(),
        };
        let msg_b = match engine_b
            .on_local_change(fresh.clone(), 60_000)
            .into_iter()
            .next()
        {
            Some(Action::Broadcast(msg)) => msg,
            other => panic!("expected Broadcast, got {:?}", other),
        };

        // Desired behavior: the newest copy in real time should win on A.
        let actions = engine_a.on_remote_update(dev_b, msg_b, 60_000);
        assert_eq!(actions, vec![Action::ApplyToClipboard(fresh)]);
    }

    #[test]
    fn remote_update_rejects_excessive_clock_drift() {
        let dev_a: DeviceId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let dev_b: DeviceId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let mut engine_a = Engine::new(dev_a);

        // Remote message claims timestamp is 70 seconds into the future (max drift is 60s)
        let msg = Message::ClipUpdate {
            update_id: uuid::Uuid::new_v4(),
            origin: dev_b,
            timestamp: HybridTimestamp::new(71_000, 0),
            content: ClipContent::Text {
                text: "future clip".to_string(),
            },
        };

        let actions = engine_a.on_remote_update(dev_b, msg, 1000);
        assert!(actions.is_empty());
        assert_eq!(engine_a.clock(), HybridTimestamp::default());
    }
}
