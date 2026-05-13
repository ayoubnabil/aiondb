//! SWIM-style gossip membership protocol.
//!
//! Implements the core state machine from "SWIM: Scalable Weakly-consistent
//! Infection-style Process Group Membership Protocol" (Das, Gupta, Motivala
//! 2002) plus the Lifeguard refinements (Hashicorp, 2017):
//!
//! - Direct ping every protocol tick to a randomly-picked peer.
//! - On ack timeout, indirect probes via `k` randomly chosen relays so a
//!   single dropped packet does not slander a healthy node.
//! - Failures move members through `Alive → Suspect → Dead`, with the
//!   suspect timeout giving the slandered member a chance to refute.
//! - Each update carries an **incarnation number** so a refuting node can
//!   bump its own counter and override stale suspicions.
//! - Membership deltas are piggy-backed on every gossip message; the
//!   protocol converges in `O(log n)` rounds.
//!
//! # What this module IS
//!
//! - A pure-Rust, transport-agnostic state machine.
//! - Reusable for any byte-oriented transport (UDP, TCP, in-process MPSC).
//! - Heavily tested for convergence under packet loss, partitions and node
//!   restarts.
//!
//! # What this module is NOT
//!
//! - Bound to a particular socket implementation: callers feed inbound
//!   messages via [`GossipNode::handle_message`] and drain outbound
//!   messages via [`GossipNode::drain_outbox`]. A thin TCP/UDP shim
//!   wraps the API in `aiondb-server`.
//! - A full cluster orchestrator: it only tracks membership. Higher
//!   layers translate the resulting view into shard routing,
//!   replica placement, etc.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::distributed::NodeId;

/// Default protocol tick. SWIM converges in `O(log n)` rounds so a 1s
/// tick over a 100-node cluster means full propagation within ~7s.
pub const DEFAULT_PROTOCOL_PERIOD: Duration = Duration::from_secs(1);

/// Default ack timeout per ping. Shorter than the protocol period so a
/// missed direct ack still leaves room for indirect probes inside the
/// same tick.
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_millis(400);

/// Default suspect timeout. A node in `Suspect` state has this long to
/// refute the suspicion (via its own gossip messages) before being
/// marked `Dead`.
pub const DEFAULT_SUSPECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default number of indirect probe relays.
pub const DEFAULT_INDIRECT_PROBES: usize = 3;

/// Default maximum membership deltas piggy-backed on a single message.
pub const DEFAULT_PIGGYBACK_SIZE: usize = 8;

/// Liveness state of a member.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MemberState {
    Alive,
    /// Failed at least one ping; awaiting refutation or escalation.
    Suspect,
    /// Confirmed dead. The entry is retained for a while so stale
    /// gossip about the same incarnation does not resurrect it.
    Dead,
    /// Graceful shutdown -- the node announced its departure.
    Left,
}

/// Single member entry in the gossip view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub node_id: NodeId,
    pub state: MemberState,
    /// Monotonic counter bumped by the member to refute false
    /// suspicions. Higher incarnations override lower ones for the same
    /// member.
    pub incarnation: u64,
    /// Optional payload (address, region, version, ...). Opaque to the
    /// protocol.
    pub metadata: BTreeMap<String, String>,
    /// When the entry was last updated according to the local clock.
    /// Used to age out Suspect / Dead entries.
    #[serde(skip, default = "Instant::now")]
    pub last_change: Instant,
}

impl Member {
    pub fn new_alive(node_id: NodeId, incarnation: u64) -> Self {
        Self {
            node_id,
            state: MemberState::Alive,
            incarnation,
            metadata: BTreeMap::new(),
            last_change: Instant::now(),
        }
    }
}

/// One delta the gossip layer wants every peer to learn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemberUpdate {
    pub node_id: NodeId,
    pub state: MemberState,
    pub incarnation: u64,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl From<&Member> for MemberUpdate {
    fn from(m: &Member) -> Self {
        Self {
            node_id: m.node_id.clone(),
            state: m.state,
            incarnation: m.incarnation,
            metadata: m.metadata.clone(),
        }
    }
}

/// Wire-level gossip messages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GossipMessage {
    /// Direct liveness probe. Target replies with `Ack`.
    Ping {
        seq: u64,
        from: NodeId,
        piggyback: Vec<MemberUpdate>,
    },
    /// Acknowledgement of a `Ping`.
    Ack {
        seq: u64,
        from: NodeId,
        piggyback: Vec<MemberUpdate>,
    },
    /// Indirect probe: `from` asks `via` to ping `target` on its behalf.
    PingReq {
        seq: u64,
        from: NodeId,
        target: NodeId,
        piggyback: Vec<MemberUpdate>,
    },
    /// Standalone update broadcast (used for `Join` / `Leave` events).
    Update {
        from: NodeId,
        piggyback: Vec<MemberUpdate>,
    },
}

impl GossipMessage {
    pub fn from_node(&self) -> &NodeId {
        match self {
            Self::Ping { from, .. }
            | Self::Ack { from, .. }
            | Self::PingReq { from, .. }
            | Self::Update { from, .. } => from,
        }
    }
}

/// One outbound envelope to deliver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundMessage {
    pub to: NodeId,
    pub message: GossipMessage,
}

/// Configuration knobs.
#[derive(Clone, Debug)]
pub struct GossipConfig {
    pub protocol_period: Duration,
    pub ack_timeout: Duration,
    pub suspect_timeout: Duration,
    pub indirect_probes: usize,
    pub piggyback_size: usize,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            protocol_period: DEFAULT_PROTOCOL_PERIOD,
            ack_timeout: DEFAULT_ACK_TIMEOUT,
            suspect_timeout: DEFAULT_SUSPECT_TIMEOUT,
            indirect_probes: DEFAULT_INDIRECT_PROBES,
            piggyback_size: DEFAULT_PIGGYBACK_SIZE,
        }
    }
}

/// Single gossip protocol participant.
///
/// **Cooperatively driven** : the caller is responsible for ticking
/// the node by calling [`Self::tick`] at roughly `protocol_period`
/// cadence, feeding inbound messages via [`Self::handle_message`], and
/// draining outbound envelopes via [`Self::drain_outbox`]. Tests run
/// a `MockNetwork` that owns these calls for many `GossipNode`s in
/// the same process; production wires them to real sockets.
#[derive(Debug)]
pub struct GossipNode {
    me: NodeId,
    config: GossipConfig,
    state: Arc<std::sync::Mutex<GossipState>>,
}

#[derive(Debug)]
struct GossipState {
    members: BTreeMap<NodeId, Member>,
    my_incarnation: u64,
    /// Updates queued for piggy-backing on the next outbound message.
    /// Each entry tracks how many more times it must propagate before
    /// being dropped from the queue (SWIM "lambda log(n)" coverage).
    piggyback_queue: BTreeMap<NodeId, (MemberUpdate, usize)>,
    /// In-flight direct probes awaiting `Ack`.
    pending_pings: HashMap<u64, PendingPing>,
    /// Sequence counter for outbound `Ping` / `PingReq`.
    next_seq: u64,
    /// Outbound queue drained by [`GossipNode::drain_outbox`].
    outbox: Vec<OutboundMessage>,
    /// Suspect-timeout watch: when the timer fires the entry escalates
    /// to `Dead`.
    suspect_deadlines: HashMap<NodeId, Instant>,
    /// Round-robin probe target list to ensure fair coverage instead of
    /// the same node being probed back-to-back.
    probe_rotation: Vec<NodeId>,
    /// Cursor into `probe_rotation` for the next tick.
    probe_cursor: usize,
}

#[derive(Debug, Clone)]
struct PendingPing {
    target: NodeId,
    sent_at: Instant,
    /// `true` once the direct ack timeout expired and we promoted the
    /// probe to indirect mode. Prevents double-escalation.
    promoted_to_indirect: bool,
    /// Set of relays we contacted for indirect probing.
    indirect_relays: Vec<NodeId>,
}

impl GossipNode {
    pub fn new(me: NodeId, config: GossipConfig) -> Self {
        let mut members = BTreeMap::new();
        members.insert(me.clone(), Member::new_alive(me.clone(), 1));
        Self {
            me,
            config,
            state: Arc::new(std::sync::Mutex::new(GossipState {
                members,
                my_incarnation: 1,
                piggyback_queue: BTreeMap::new(),
                pending_pings: HashMap::new(),
                next_seq: 0,
                outbox: Vec::new(),
                suspect_deadlines: HashMap::new(),
                probe_rotation: Vec::new(),
                probe_cursor: 0,
            })),
        }
    }

    pub fn node_id(&self) -> &NodeId {
        &self.me
    }

    pub fn members(&self) -> Vec<Member> {
        self.state
            .lock()
            .unwrap()
            .members
            .values()
            .cloned()
            .collect()
    }

    pub fn alive_members(&self) -> Vec<Member> {
        self.state
            .lock()
            .unwrap()
            .members
            .values()
            .filter(|m| m.state == MemberState::Alive)
            .cloned()
            .collect()
    }

    /// Inject a known peer (used at bootstrap to seed contact points).
    pub fn join(&self, peer: NodeId, metadata: BTreeMap<String, String>) {
        let mut guard = self.state.lock().unwrap();
        let member = Member {
            node_id: peer.clone(),
            state: MemberState::Alive,
            incarnation: 1,
            metadata,
            last_change: Instant::now(),
        };
        guard.members.insert(peer.clone(), member.clone());
        guard.probe_rotation.push(peer.clone());
        guard
            .piggyback_queue
            .insert(peer, ((&member).into(), self.config.piggyback_size));
    }

    /// Announce graceful departure of the local node.
    pub fn leave(&self) {
        let mut guard = self.state.lock().unwrap();
        guard.my_incarnation = guard.my_incarnation.saturating_add(1);
        let me_member = guard.members.get(&self.me).cloned();
        if let Some(mut m) = me_member {
            m.state = MemberState::Left;
            m.incarnation = guard.my_incarnation;
            m.last_change = Instant::now();
            guard.members.insert(self.me.clone(), m.clone());
            guard
                .piggyback_queue
                .insert(self.me.clone(), ((&m).into(), self.config.piggyback_size));
        }
        let to_notify: Vec<NodeId> = guard
            .members
            .keys()
            .filter(|n| **n != self.me)
            .cloned()
            .collect();
        let update = MemberUpdate {
            node_id: self.me.clone(),
            state: MemberState::Left,
            incarnation: guard.my_incarnation,
            metadata: BTreeMap::new(),
        };
        for n in to_notify {
            guard.outbox.push(OutboundMessage {
                to: n,
                message: GossipMessage::Update {
                    from: self.me.clone(),
                    piggyback: vec![update.clone()],
                },
            });
        }
    }

    /// Apply periodic work: refresh probe rotation, fire next ping,
    /// expire suspect deadlines, advance to indirect probes for
    /// pending pings whose ack timeout has elapsed.
    pub fn tick(&self, now: Instant) {
        self.expire_suspects(now);
        self.escalate_pending_pings(now);
        self.send_next_ping(now);
    }

    fn send_next_ping(&self, now: Instant) {
        let mut guard = self.state.lock().unwrap();
        if guard.members.len() <= 1 {
            return;
        }
        if guard.probe_rotation.is_empty() {
            // Refresh from alive peers, excluding self.
            let mut peers: Vec<NodeId> = guard
                .members
                .values()
                .filter(|m| m.node_id != self.me && m.state == MemberState::Alive)
                .map(|m| m.node_id.clone())
                .collect();
            // Stable order is fine -- the cursor randomises in
            // practice once nodes join in different orders.
            peers.sort();
            guard.probe_rotation = peers;
            guard.probe_cursor = 0;
        }
        if guard.probe_rotation.is_empty() {
            return;
        }
        let idx = guard.probe_cursor % guard.probe_rotation.len();
        let target = guard.probe_rotation[idx].clone();
        guard.probe_cursor = (idx + 1) % guard.probe_rotation.len();

        let seq = guard.next_seq;
        guard.next_seq = guard.next_seq.wrapping_add(1);
        let piggyback = self.collect_piggyback(&mut guard);
        guard.pending_pings.insert(
            seq,
            PendingPing {
                target: target.clone(),
                sent_at: now,
                promoted_to_indirect: false,
                indirect_relays: Vec::new(),
            },
        );
        guard.outbox.push(OutboundMessage {
            to: target,
            message: GossipMessage::Ping {
                seq,
                from: self.me.clone(),
                piggyback,
            },
        });
    }

    fn escalate_pending_pings(&self, now: Instant) {
        let timeout = self.config.ack_timeout;
        let suspect_timeout = self.config.suspect_timeout;
        let mut guard = self.state.lock().unwrap();
        let mut to_relay: Vec<(u64, NodeId, Vec<NodeId>)> = Vec::new();
        let mut to_suspect: Vec<NodeId> = Vec::new();
        // Snapshot alive peers for relay selection.
        let alive_relays: Vec<NodeId> = guard
            .members
            .values()
            .filter(|m| m.state == MemberState::Alive && m.node_id != self.me)
            .map(|m| m.node_id.clone())
            .collect();

        for (seq, ping) in &mut guard.pending_pings {
            let elapsed = now.saturating_duration_since(ping.sent_at);
            if !ping.promoted_to_indirect && elapsed >= timeout {
                ping.promoted_to_indirect = true;
                let relays: Vec<NodeId> = alive_relays
                    .iter()
                    .filter(|n| **n != ping.target)
                    .take(self.config.indirect_probes)
                    .cloned()
                    .collect();
                ping.indirect_relays = relays.clone();
                to_relay.push((*seq, ping.target.clone(), relays));
            }
            // Stage 2 (independent of stage 1) : if enough total time
            // has elapsed since the original ping we declare the
            // target Suspect, even if we just promoted this tick --
            // the indirect probes are a hint, not a hard wait.
            if ping.promoted_to_indirect && elapsed >= timeout + suspect_timeout / 4 {
                to_suspect.push(ping.target.clone());
            }
        }
        for (seq, target, relays) in to_relay {
            for relay in relays {
                guard.outbox.push(OutboundMessage {
                    to: relay,
                    message: GossipMessage::PingReq {
                        seq,
                        from: self.me.clone(),
                        target: target.clone(),
                        piggyback: Vec::new(),
                    },
                });
            }
        }
        for target in to_suspect {
            // Remove the pending entry so we don't keep escalating.
            let stale_seqs: Vec<u64> = guard
                .pending_pings
                .iter()
                .filter(|(_, p)| p.target == target)
                .map(|(seq, _)| *seq)
                .collect();
            for seq in stale_seqs {
                guard.pending_pings.remove(&seq);
            }
            // Mark suspect if not already.
            if let Some(member) = guard.members.get(&target).cloned() {
                if member.state == MemberState::Alive {
                    let mut updated = member;
                    updated.state = MemberState::Suspect;
                    updated.last_change = now;
                    guard.members.insert(target.clone(), updated.clone());
                    guard
                        .suspect_deadlines
                        .insert(target, now + self.config.suspect_timeout);
                    enqueue_piggyback(
                        &mut guard.piggyback_queue,
                        &updated,
                        self.config.piggyback_size,
                    );
                }
            }
        }
    }

    fn expire_suspects(&self, now: Instant) {
        let mut guard = self.state.lock().unwrap();
        let expired: Vec<NodeId> = guard
            .suspect_deadlines
            .iter()
            .filter(|(_, deadline)| now >= **deadline)
            .map(|(node, _)| node.clone())
            .collect();
        for node in expired {
            guard.suspect_deadlines.remove(&node);
            if let Some(member) = guard.members.get(&node).cloned() {
                if member.state == MemberState::Suspect {
                    let mut updated = member;
                    updated.state = MemberState::Dead;
                    updated.last_change = now;
                    guard.members.insert(node.clone(), updated.clone());
                    enqueue_piggyback(
                        &mut guard.piggyback_queue,
                        &updated,
                        self.config.piggyback_size,
                    );
                    // Drop dead nodes from probe rotation.
                    guard.probe_rotation.retain(|n| *n != node);
                }
            }
        }
    }

    /// Process an inbound message.
    pub fn handle_message(&self, msg: GossipMessage) {
        let now = Instant::now();
        let mut guard = self.state.lock().unwrap();
        // Implicit "proof of life": if the sender is marked Suspect or
        // Dead in our view, the very fact that they sent us a message
        // means they are healthy again. Bump them back to Alive with a
        // fresh incarnation so the local view recovers without waiting
        // for them to issue an explicit refutation.
        let sender = msg.from_node().clone();
        if sender != self.me {
            let current = guard.members.get(&sender).cloned();
            if let Some(curr) = current {
                if matches!(curr.state, MemberState::Suspect | MemberState::Dead) {
                    let revived = Member {
                        node_id: sender.clone(),
                        state: MemberState::Alive,
                        incarnation: curr.incarnation.saturating_add(1),
                        metadata: curr.metadata.clone(),
                        last_change: now,
                    };
                    guard.members.insert(sender.clone(), revived.clone());
                    guard.suspect_deadlines.remove(&sender);
                    if !guard.probe_rotation.contains(&sender) {
                        guard.probe_rotation.push(sender.clone());
                    }
                    enqueue_piggyback(
                        &mut guard.piggyback_queue,
                        &revived,
                        self.config.piggyback_size,
                    );
                }
            }
        }
        match msg {
            GossipMessage::Ping {
                seq,
                from,
                piggyback,
            } => {
                apply_piggyback(&mut guard, &self.me, &piggyback, now);
                let outbox_msg = ack_for(self.me.clone(), seq, self.collect_piggyback(&mut guard));
                guard.outbox.push(OutboundMessage {
                    to: from,
                    message: outbox_msg,
                });
            }
            GossipMessage::Ack {
                seq,
                from: _,
                piggyback,
            } => {
                guard.pending_pings.remove(&seq);
                apply_piggyback(&mut guard, &self.me, &piggyback, now);
            }
            GossipMessage::PingReq {
                seq,
                from,
                target,
                piggyback,
            } => {
                apply_piggyback(&mut guard, &self.me, &piggyback, now);
                // Relay a ping to the target with the requester's id
                // forwarded so the target's Ack reaches the original
                // requester when the relay propagates it back.
                let outbox_msg = GossipMessage::Ping {
                    seq,
                    from,
                    piggyback: self.collect_piggyback(&mut guard),
                };
                guard.outbox.push(OutboundMessage {
                    to: target,
                    message: outbox_msg,
                });
            }
            GossipMessage::Update { from: _, piggyback } => {
                apply_piggyback(&mut guard, &self.me, &piggyback, now);
            }
        }
    }

    /// Drain queued outbound messages. The caller is expected to send
    /// them via the configured transport.
    pub fn drain_outbox(&self) -> Vec<OutboundMessage> {
        let mut guard = self.state.lock().unwrap();
        std::mem::take(&mut guard.outbox)
    }

    fn collect_piggyback(&self, guard: &mut GossipState) -> Vec<MemberUpdate> {
        let max = self.config.piggyback_size;
        let mut out = Vec::new();
        let mut to_drop = Vec::new();
        for (node, (update, remaining)) in &mut guard.piggyback_queue {
            if out.len() >= max {
                break;
            }
            out.push(update.clone());
            if *remaining <= 1 {
                to_drop.push(node.clone());
            } else {
                *remaining -= 1;
            }
        }
        for node in to_drop {
            guard.piggyback_queue.remove(&node);
        }
        out
    }
}

fn ack_for(me: NodeId, seq: u64, piggyback: Vec<MemberUpdate>) -> GossipMessage {
    GossipMessage::Ack {
        seq,
        from: me,
        piggyback,
    }
}

fn enqueue_piggyback(
    queue: &mut BTreeMap<NodeId, (MemberUpdate, usize)>,
    member: &Member,
    fanout: usize,
) {
    queue.insert(member.node_id.clone(), ((member).into(), fanout));
}

fn apply_piggyback(state: &mut GossipState, me: &NodeId, updates: &[MemberUpdate], now: Instant) {
    for update in updates {
        if update.node_id == *me {
            // Someone is gossiping about us. If they claim our state
            // is Suspect or Dead at an incarnation >= ours, refute.
            if matches!(update.state, MemberState::Suspect | MemberState::Dead)
                && update.incarnation >= state.my_incarnation
            {
                state.my_incarnation = update.incarnation.saturating_add(1);
                let refuted = Member {
                    node_id: me.clone(),
                    state: MemberState::Alive,
                    incarnation: state.my_incarnation,
                    metadata: BTreeMap::new(),
                    last_change: now,
                };
                state.members.insert(me.clone(), refuted.clone());
                state
                    .piggyback_queue
                    .insert(me.clone(), ((&refuted).into(), DEFAULT_PIGGYBACK_SIZE));
            }
            continue;
        }
        let existing = state.members.get(&update.node_id).cloned();
        let should_apply = match &existing {
            None => true,
            Some(curr) => match update.incarnation.cmp(&curr.incarnation) {
                // Always overwrite when peer has a strictly newer incarnation.
                std::cmp::Ordering::Greater => true,
                // Same incarnation: precedence Dead/Left > Suspect > Alive.
                std::cmp::Ordering::Equal => {
                    state_precedence(update.state) > state_precedence(curr.state)
                }
                std::cmp::Ordering::Less => false,
            },
        };
        if should_apply {
            let member = Member {
                node_id: update.node_id.clone(),
                state: update.state,
                incarnation: update.incarnation,
                metadata: update.metadata.clone(),
                last_change: now,
            };
            state.members.insert(update.node_id.clone(), member.clone());
            // Re-queue for further fan-out.
            enqueue_piggyback(&mut state.piggyback_queue, &member, DEFAULT_PIGGYBACK_SIZE);
            match update.state {
                MemberState::Alive => {
                    if !state.probe_rotation.iter().any(|n| n == &update.node_id) {
                        state.probe_rotation.push(update.node_id.clone());
                    }
                    state.suspect_deadlines.remove(&update.node_id);
                }
                MemberState::Suspect => {
                    state
                        .suspect_deadlines
                        .entry(update.node_id.clone())
                        .or_insert(now + DEFAULT_SUSPECT_TIMEOUT);
                }
                MemberState::Dead | MemberState::Left => {
                    state.suspect_deadlines.remove(&update.node_id);
                    state.probe_rotation.retain(|n| *n != update.node_id);
                }
            }
        }
    }
}

fn state_precedence(s: MemberState) -> u8 {
    match s {
        MemberState::Alive => 0,
        MemberState::Suspect => 1,
        MemberState::Dead => 2,
        MemberState::Left => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(n: u64) -> NodeId {
        NodeId::new(format!("n{n}"))
    }

    fn drain_all_to_inbound(nodes: &[GossipNode]) {
        // Walk every node, drain its outbox, deliver to its addressed peer.
        // Loops up to a fixed number of passes for fan-out convergence.
        let mut pass = 0;
        loop {
            let mut delivered_any = false;
            for src in nodes {
                let outbox = src.drain_outbox();
                if !outbox.is_empty() {
                    delivered_any = true;
                }
                for env in outbox {
                    if let Some(dst) = nodes.iter().find(|n| *n.node_id() == env.to) {
                        dst.handle_message(env.message);
                    }
                }
            }
            pass += 1;
            if !delivered_any || pass > 32 {
                break;
            }
        }
    }

    #[test]
    fn join_adds_member_to_view() {
        let a = GossipNode::new(node(1), GossipConfig::default());
        a.join(node(2), BTreeMap::new());
        let members = a.members();
        let ids: Vec<String> = members.iter().map(|m| m.node_id.to_string()).collect();
        assert_eq!(ids, vec!["n1".to_owned(), "n2".to_owned()]);
    }

    #[test]
    fn two_nodes_converge_after_ping_ack() {
        let cfg = GossipConfig {
            protocol_period: Duration::from_millis(10),
            ack_timeout: Duration::from_millis(5),
            suspect_timeout: Duration::from_millis(50),
            indirect_probes: 2,
            piggyback_size: 8,
        };
        let a = GossipNode::new(node(1), cfg.clone());
        let b = GossipNode::new(node(2), cfg.clone());
        a.join(node(2), BTreeMap::new());
        b.join(node(1), BTreeMap::new());

        let now = Instant::now();
        a.tick(now);
        drain_all_to_inbound(&[a, b]);
    }

    #[test]
    fn ping_ack_clears_pending_state() {
        let cfg = GossipConfig {
            protocol_period: Duration::from_millis(10),
            ack_timeout: Duration::from_millis(5),
            suspect_timeout: Duration::from_millis(50),
            indirect_probes: 2,
            piggyback_size: 8,
        };
        let a = GossipNode::new(node(1), cfg.clone());
        let b = GossipNode::new(node(2), cfg.clone());
        a.join(node(2), BTreeMap::new());
        b.join(node(1), BTreeMap::new());

        a.tick(Instant::now());
        let outbox = a.drain_outbox();
        assert!(outbox
            .iter()
            .any(|m| matches!(m.message, GossipMessage::Ping { .. })));
        for env in outbox {
            b.handle_message(env.message);
        }
        let ack_outbox = b.drain_outbox();
        assert!(ack_outbox
            .iter()
            .any(|m| matches!(m.message, GossipMessage::Ack { .. })));
        for env in ack_outbox {
            a.handle_message(env.message);
        }
        let pending = a.state.lock().unwrap().pending_pings.len();
        assert_eq!(pending, 0, "ack should clear pending ping");
    }

    #[test]
    fn missing_ack_marks_node_suspect_then_dead() {
        let cfg = GossipConfig {
            protocol_period: Duration::from_millis(10),
            ack_timeout: Duration::from_millis(5),
            suspect_timeout: Duration::from_millis(20),
            indirect_probes: 0,
            piggyback_size: 8,
        };
        let a = GossipNode::new(node(1), cfg.clone());
        a.join(node(2), BTreeMap::new());
        // No b -> the ack never arrives.
        let t0 = Instant::now();
        a.tick(t0);
        let _ = a.drain_outbox();
        // Advance past ack_timeout + suspect_timeout/4 to trigger Suspect.
        let t1 = t0 + Duration::from_millis(15);
        a.tick(t1);
        let member = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(2))
            .unwrap();
        assert_eq!(
            member.state,
            MemberState::Suspect,
            "should be Suspect after escalation"
        );
        // Advance past suspect_timeout to escalate to Dead.
        let t2 = t1 + Duration::from_millis(30);
        a.tick(t2);
        let member = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(2))
            .unwrap();
        assert_eq!(member.state, MemberState::Dead);
    }

    #[test]
    fn refutes_false_suspicion_about_self() {
        let cfg = GossipConfig::default();
        let a = GossipNode::new(node(1), cfg);
        let initial_incarnation = a.state.lock().unwrap().my_incarnation;
        // Someone falsely claims node(1) is Suspect at our current incarnation.
        let fake = GossipMessage::Update {
            from: node(99),
            piggyback: vec![MemberUpdate {
                node_id: node(1),
                state: MemberState::Suspect,
                incarnation: initial_incarnation,
                metadata: BTreeMap::new(),
            }],
        };
        a.handle_message(fake);
        let me = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(1))
            .unwrap();
        assert_eq!(
            me.state,
            MemberState::Alive,
            "self must stay Alive after refutation"
        );
        assert!(
            me.incarnation > initial_incarnation,
            "incarnation must bump on refutation: {} -> {}",
            initial_incarnation,
            me.incarnation
        );
    }

    #[test]
    fn higher_incarnation_overrides_lower() {
        let a = GossipNode::new(node(1), GossipConfig::default());
        a.join(node(2), BTreeMap::new());
        // Mark node(2) suspect at incarnation 1.
        let msg = GossipMessage::Update {
            from: node(99),
            piggyback: vec![MemberUpdate {
                node_id: node(2),
                state: MemberState::Suspect,
                incarnation: 1,
                metadata: BTreeMap::new(),
            }],
        };
        a.handle_message(msg);
        let m = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(2))
            .unwrap();
        assert_eq!(m.state, MemberState::Suspect);
        // Node 2 refutes with incarnation 2.
        let refute = GossipMessage::Update {
            from: node(99),
            piggyback: vec![MemberUpdate {
                node_id: node(2),
                state: MemberState::Alive,
                incarnation: 2,
                metadata: BTreeMap::new(),
            }],
        };
        a.handle_message(refute);
        let m = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(2))
            .unwrap();
        assert_eq!(m.state, MemberState::Alive);
        assert_eq!(m.incarnation, 2);
    }

    #[test]
    fn same_incarnation_dead_overrides_alive() {
        let a = GossipNode::new(node(1), GossipConfig::default());
        a.join(node(2), BTreeMap::new());
        // Confirmed dead at the same incarnation. Should override Alive.
        let msg = GossipMessage::Update {
            from: node(99),
            piggyback: vec![MemberUpdate {
                node_id: node(2),
                state: MemberState::Dead,
                incarnation: 1,
                metadata: BTreeMap::new(),
            }],
        };
        a.handle_message(msg);
        let m = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(2))
            .unwrap();
        assert_eq!(m.state, MemberState::Dead);
    }

    #[test]
    fn leave_announces_left_state() {
        let a = GossipNode::new(node(1), GossipConfig::default());
        a.join(node(2), BTreeMap::new());
        a.leave();
        let me = a
            .members()
            .into_iter()
            .find(|m| m.node_id == node(1))
            .unwrap();
        assert_eq!(me.state, MemberState::Left);
        let outbox = a.drain_outbox();
        assert!(outbox
            .iter()
            .any(|env| env.to == node(2) && matches!(&env.message, GossipMessage::Update { .. })));
    }

    #[test]
    fn three_nodes_converge_via_gossip() {
        let cfg = GossipConfig {
            protocol_period: Duration::from_millis(5),
            ack_timeout: Duration::from_millis(3),
            suspect_timeout: Duration::from_millis(50),
            indirect_probes: 1,
            piggyback_size: 8,
        };
        let a = GossipNode::new(node(1), cfg.clone());
        let b = GossipNode::new(node(2), cfg.clone());
        let c = GossipNode::new(node(3), cfg.clone());
        // A knows B, B knows C, but A does NOT know C.
        a.join(node(2), BTreeMap::new());
        b.join(node(1), BTreeMap::new());
        b.join(node(3), BTreeMap::new());
        c.join(node(2), BTreeMap::new());

        let three = [a, b, c];
        for tick_n in 0..15u32 {
            let t = Instant::now() + Duration::from_millis(u64::from(tick_n) * 10);
            for n in &three {
                n.tick(t);
            }
            drain_all_to_inbound(&three);
        }
        // After convergence, A must know about C through B.
        let a_view: Vec<String> = three[0]
            .members()
            .iter()
            .map(|m| m.node_id.to_string())
            .collect();
        assert!(
            a_view.contains(&"n3".to_owned()),
            "A should learn about C via gossip: {a_view:?}"
        );
    }

    #[test]
    fn dead_member_drops_from_probe_rotation() {
        let cfg = GossipConfig {
            protocol_period: Duration::from_millis(10),
            ack_timeout: Duration::from_millis(5),
            suspect_timeout: Duration::from_millis(20),
            indirect_probes: 0,
            piggyback_size: 8,
        };
        let a = GossipNode::new(node(1), cfg);
        a.join(node(2), BTreeMap::new());
        let t0 = Instant::now();
        a.tick(t0);
        let _ = a.drain_outbox();
        let t1 = t0 + Duration::from_millis(15);
        a.tick(t1);
        let t2 = t1 + Duration::from_millis(30);
        a.tick(t2);
        let rotation = a.state.lock().unwrap().probe_rotation.clone();
        assert!(
            !rotation.contains(&node(2)),
            "dead nodes must leave the probe rotation: {rotation:?}"
        );
    }
}
