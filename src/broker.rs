//! 所有可变状态只由一个 Mach 事件循环修改。
//! 待处理请求仅保存连接标识，不延长已断开的端口生命周期。

use crate::mach::{
    self, Event, Message, PortSet, ReceiveRight, ReplyPort, ReplyRight, SendOnceRight, SendRight,
};
use crate::process;
use crate::protocol::{AuditToken, Connection, Operation, MAX_BODY};
use crate::ring::{LogRecord, LogRing, BYTE_LIMIT, MAX_CAPACITY};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::time::{Duration, Instant};

const REQUEST_LIMIT: usize = 64;
const SUBSCRIBER_LIMIT: usize = 16;
const DELIVERY_BUDGET: usize = 8;
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_TIMEOUT_MS: u32 = 100;

fn request_available(count: usize, next: u64) -> bool {
    count < REQUEST_LIMIT && next < u64::MAX
}

fn subscriber_available(count: usize) -> bool {
    count < SUBSCRIBER_LIMIT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PeerKey {
    pid: i32,
    generation: u64,
}

#[derive(Default)]
struct PeerTrust {
    audit: Option<AuditToken>,
    ready: bool,
}

impl PeerTrust {
    fn accepts(&self, pid: i32, audit: AuditToken) -> bool {
        audit.pid() == pid && self.audit.is_none_or(|expected| expected == audit)
    }

    fn hello(&mut self, audit: AuditToken, body: &Value) -> bool {
        if !bool_value(&body["hooks_ready"]) {
            return false;
        }
        self.audit = Some(audit);
        self.ready = true;
        true
    }

    fn handshake_expired(&self, deadline: Instant, now: Instant) -> bool {
        !self.ready && now >= deadline
    }
}

struct Peer {
    key: PeerKey,
    send: SendRight,
    receive: ReceiveRight,
    trust: PeerTrust,
    hello_deadline: Instant,
}

impl Drop for Peer {
    fn drop(&mut self) {
        // 先取消通知，再让拥有的 send/receive right 自动释放。
        let _ = self.send.unwatch_dead();
    }
}

struct RequestState {
    identifier: u64,
    operation: Operation,
    deadline: Instant,
    peers: Vec<PeerKey>,
    results: BTreeMap<i32, Value>,
    errors: BTreeMap<i32, Value>,
}

impl RequestState {
    fn new(
        identifier: u64,
        operation: Operation,
        peers: Vec<PeerKey>,
        now: Instant,
        errors: BTreeMap<i32, Value>,
    ) -> Self {
        Self {
            identifier,
            operation,
            deadline: now + REQUEST_TIMEOUT,
            peers,
            results: BTreeMap::new(),
            errors,
        }
    }

    fn wire_operation(&self) -> Operation {
        if self.operation == Operation::Attach {
            Operation::Status
        } else {
            self.operation
        }
    }

    fn record(&mut self, peer: PeerKey, operation: Operation, body: Value) {
        // 同一个 PID 的新连接不能回复旧连接的请求。
        if !self.peers.contains(&peer) {
            return;
        }
        if operation == Operation::Error {
            self.errors.insert(
                peer.pid,
                body.get("error").cloned().unwrap_or(json!("target_error")),
            );
        } else if operation == self.wire_operation() {
            if !bool_value(&body["hooks_ready"]) {
                self.errors.insert(peer.pid, json!("injection_invalid"));
            } else {
                self.results.insert(peer.pid, body);
            }
        }
    }

    fn disconnected(&mut self, peer: PeerKey, reason: &str) {
        if self.peers.contains(&peer) && !self.results.contains_key(&peer.pid) {
            self.errors.insert(peer.pid, json!(reason));
        }
    }

    fn complete(&self) -> bool {
        self.peers
            .iter()
            .all(|peer| self.results.contains_key(&peer.pid) || self.errors.contains_key(&peer.pid))
    }

    fn finish_if_due(&mut self, now: Instant) -> bool {
        if !self.complete() && now < self.deadline {
            return false;
        }
        for peer in &self.peers {
            if !self.results.contains_key(&peer.pid) && !self.errors.contains_key(&peer.pid) {
                self.errors.insert(peer.pid, json!("timeout"));
            }
        }
        true
    }

    fn summary(
        &self,
        live: Option<&[i32]>,
        connected: &[i32],
        helper_pid: u32,
        session: &str,
        ring: Value,
    ) -> (Operation, Value) {
        let live_pids = live.unwrap_or(&[]);
        let missing: Vec<i32> = live_pids
            .iter()
            .copied()
            .filter(|pid| !connected.contains(pid))
            .collect();
        let results: Vec<Value> = self
            .peers
            .iter()
            .filter_map(|peer| self.results.get(&peer.pid).cloned())
            .collect();
        let errors: Vec<Value> = self
            .errors
            .iter()
            .map(|(pid, error)| json!({"pid": pid, "error": error}))
            .collect();
        let snapshot_pids: BTreeSet<i32> = self.peers.iter().map(|peer| peer.pid).collect();
        let expected_pids: BTreeSet<i32> = live_pids.iter().copied().collect();
        let ok = live.is_some()
            && !live_pids.is_empty()
            && self.errors.is_empty()
            && missing.is_empty()
            && results.len() == live_pids.len()
            && results.len() == self.peers.len()
            && snapshot_pids == expected_pids;
        let enabled = ok
            && !self.peers.is_empty()
            && results.iter().all(|state| bool_value(&state["enabled"]));
        let mut body = json!({
            "ok": ok,
            "enabled": enabled,
            "helper_pid": helper_pid,
            "helper_session": session,
            "request_id": self.identifier,
            "connected": connected.len(),
            "expected": live_pids.len(),
            "acknowledged": results.len(),
            "results": results,
            "errors": errors,
            "missing_pids": missing,
            "ring": ring,
        });
        if live.is_none() {
            body["error"] = json!("process_discovery_failed");
        } else if !ok {
            body["error"] = json!("incomplete_or_stale_connections");
        }
        (if ok { self.operation } else { Operation::Error }, body)
    }
}

struct PendingRequest {
    state: RequestState,
    // send(self, ...) 消费此 right。取消请求也会自动释放它。
    reply: SendOnceRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Advance {
    Begin,
    ReplayEnd,
    Cursor(u64),
}

struct Delivery {
    operation: Operation,
    request: u64,
    body: Value,
    advance: Advance,
}

#[derive(Debug)]
struct SubscriptionState {
    cursor: u64,
    boundary: u64,
    began: bool,
    replay_ended: bool,
    follow: bool,
}

impl SubscriptionState {
    fn new(ring: &LogRing, snapshot: bool) -> Self {
        Self {
            cursor: ring.first_sequence(),
            boundary: ring.last_sequence(),
            began: false,
            replay_ended: false,
            follow: !snapshot,
        }
    }

    /// 这里只准备帧。投递成功前不移动游标。
    fn next(&self, ring: &LogRing, session: &str, oversized: u64) -> Option<Delivery> {
        let (operation, request, body, advance) = if !self.began {
            (
                Operation::ReplayBegin,
                0,
                json!({
                    "type": "replay_begin",
                    "helper_session": session,
                    "first_sequence": self.cursor,
                    "snapshot_end": self.boundary,
                    "ring": ring.state(oversized),
                }),
                Advance::Begin,
            )
        } else if !self.replay_ended && self.cursor > self.boundary {
            (
                Operation::ReplayEnd,
                0,
                json!({
                    "type": "replay_end", "snapshot_end": self.boundary,
                }),
                Advance::ReplayEnd,
            )
        } else if self.cursor < ring.first_sequence() {
            let mut end = ring.first_sequence() - 1;
            if !self.replay_ended {
                end = end.min(self.boundary);
            }
            (
                Operation::Gap,
                0,
                json!({
                    "type": "gap", "from": self.cursor, "to": end, "reason": "ring_evicted",
                }),
                Advance::Cursor(end + 1),
            )
        } else {
            let entry = ring.entry(self.cursor)?;
            (
                Operation::BypassLog,
                entry.sequence,
                entry.body.clone(),
                Advance::Cursor(entry.sequence + 1),
            )
        };
        Some(Delivery {
            operation,
            request,
            body,
            advance,
        })
    }

    /// 返回 true 时，快照订阅已经完成，应释放 send right。
    fn delivered(&mut self, advance: Advance) -> bool {
        match advance {
            Advance::Begin => self.began = true,
            Advance::ReplayEnd => self.replay_ended = true,
            Advance::Cursor(cursor) => self.cursor = cursor,
        }
        self.replay_ended && !self.follow
    }
}

struct Subscriber {
    send: SendRight,
    state: SubscriptionState,
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        let _ = self.send.unwatch_dead();
    }
}

struct Broker {
    peers: BTreeMap<i32, Peer>,
    receivers: BTreeMap<u32, i32>,
    requests: BTreeMap<u64, PendingRequest>,
    subscribers: BTreeMap<u32, Subscriber>,
    ring: LogRing,
    session: String,
    next_request: u64,
    next_peer: u64,
    oversized: u64,
    had_peers: bool,
    clients: ReceiveRight,
    notifications: ReceiveRight,
    port_set: PortSet,
}

impl Broker {
    fn new(capacity: usize) -> Result<Self> {
        let clients = ReceiveRight::new().context("client_receive_port_failed")?;
        let notifications = ReceiveRight::new().context("notification_receive_port_failed")?;
        let port_set = PortSet::new().context("port_set_failed")?;
        port_set.add(&clients)?;
        port_set.add(&notifications)?;
        let session = new_session()?;
        mach::register_service(&clients).context("service_registration_failed")?;
        Ok(Self {
            peers: BTreeMap::new(),
            receivers: BTreeMap::new(),
            requests: BTreeMap::new(),
            subscribers: BTreeMap::new(),
            ring: LogRing::new(capacity, BYTE_LIMIT),
            session,
            next_request: 1,
            next_peer: 1,
            oversized: 0,
            had_peers: false,
            clients,
            notifications,
            port_set,
        })
    }

    fn add_connection(&mut self, connection: Connection) -> std::result::Result<(), &'static str> {
        if connection.pid <= 0
            || connection.receive == 0
            || process::verify_tccd(connection.pid).is_err()
        {
            return Err("target_is_not_tccd");
        }
        if self.peers.contains_key(&connection.pid) {
            return Ok(()); // 重复启动不重置握手或 audit token。
        }
        let next_peer = self.next_peer.checked_add(1).ok_or("connection_limit")?;
        let send = mach::extract_send(connection.pid, connection.receive)
            .map_err(|_| "port_handoff_failed")?;
        let receive = ReceiveRight::new().map_err(|_| "receive_port_failed")?;
        self.port_set
            .add(&receive)
            .map_err(|_| "receive_port_failed")?;
        send.watch_dead(&self.notifications)
            .map_err(|_| "death_watch_failed")?;
        let peer = Peer {
            key: PeerKey {
                pid: connection.pid,
                generation: self.next_peer,
            },
            send,
            receive,
            trust: PeerTrust::default(),
            hello_deadline: Instant::now() + HELLO_TIMEOUT,
        };
        self.next_peer = next_peer;
        self.had_peers = true;
        let sent = peer.send.send(
            Operation::Hello,
            0,
            &json!({}),
            ReplyPort::Send(&peer.receive),
            SEND_TIMEOUT_MS,
        );
        self.receivers.insert(peer.receive.name(), peer.key.pid);
        self.peers.insert(peer.key.pid, peer);
        if sent.is_err() {
            self.remove_peer(connection.pid, "handshake_send_failed");
            return Err("handshake_send_failed");
        }
        Ok(())
    }

    fn remove_peer(&mut self, pid: i32, reason: &str) {
        if let Some(peer) = self.peers.remove(&pid) {
            self.receivers.remove(&peer.receive.name());
            for request in self.requests.values_mut() {
                request.state.disconnected(peer.key, reason);
            }
            crate::lifecycle::log(&format!("tccd disconnected pid={pid} reason={reason}"));
            // peer 的 Drop 取消死亡通知，随后释放两个端口。
        }
    }

    fn handle_death(&mut self, name: u32) {
        // receive() 已释放通知额外添加的 dead-name 引用。
        if self.subscribers.remove(&name).is_some() {
            return;
        }
        let pid = self
            .peers
            .values()
            .find(|peer| peer.send.name() == name)
            .map(|peer| peer.key.pid);
        if let Some(pid) = pid {
            self.remove_peer(pid, "port_dead");
        }
    }

    fn begin_request(
        &mut self,
        operation: Operation,
        body: Value,
        reply: SendOnceRight,
        errors: BTreeMap<i32, Value>,
    ) {
        if !request_available(self.requests.len(), self.next_request) {
            one_reply(
                reply,
                Operation::Error,
                0,
                json!({"ok": false, "error": "busy"}),
            );
            return;
        }
        let identifier = self.next_request;
        self.next_request += 1;
        let peers = self.peers.values().map(|peer| peer.key).collect();
        let mut state = RequestState::new(identifier, operation, peers, Instant::now(), errors);
        let wire_operation = state.wire_operation();
        for peer in self.peers.values() {
            if peer
                .send
                .send(
                    wire_operation,
                    identifier,
                    &body,
                    ReplyPort::None,
                    SEND_TIMEOUT_MS,
                )
                .is_err()
            {
                state.errors.insert(peer.key.pid, json!("send_failed"));
            }
        }
        self.requests
            .insert(identifier, PendingRequest { state, reply });
    }

    fn handle_client(&mut self, mut message: Message) {
        if message.operation == Operation::Subscribe {
            // 不接受 send-once 流，也不把 send right 当作单次回复。
            let Some(ReplyRight::Send(send)) = message.reply.take() else {
                return;
            };
            if message.audit.euid() != 0 {
                end_stream(&send, "error", "root_required");
                return;
            }
            if !message.body.is_object() {
                return;
            }
            if !subscriber_available(self.subscribers.len()) {
                end_stream(&send, "error", "too_many_subscribers");
                return;
            }
            // 先销毁旧订阅，避免它取消新订阅的死亡通知。
            self.subscribers.remove(&send.name());
            if send.watch_dead(&self.notifications).is_err() {
                end_stream(&send, "error", "death_watch_failed");
                return;
            }
            let subscriber = Subscriber {
                send,
                state: SubscriptionState::new(&self.ring, bool_value(&message.body["snapshot"])),
            };
            self.subscribers.insert(subscriber.send.name(), subscriber);
            return;
        }
        let Some(ReplyRight::Once(reply)) = message.reply.take() else {
            return;
        };
        if message.audit.euid() != 0 {
            one_reply(
                reply,
                Operation::Error,
                0,
                json!({"ok": false, "error": "root_required"}),
            );
            return;
        }
        if !message.body.is_object() {
            one_reply(
                reply,
                Operation::Error,
                0,
                json!({"ok": false, "error": "invalid_request"}),
            );
            return;
        }
        match message.operation {
            Operation::Attach => {
                let Some(connections) = message.body["connections"].as_array() else {
                    one_reply(
                        reply,
                        Operation::Error,
                        0,
                        json!({"ok": false, "error": "invalid_connections"}),
                    );
                    return;
                };
                let mut errors = BTreeMap::new();
                for value in connections {
                    let error_pid = value
                        .get("pid")
                        .and_then(Value::as_i64)
                        .and_then(|pid| i32::try_from(pid).ok())
                        .unwrap_or(0);
                    let result = serde_json::from_value::<Connection>(value.clone())
                        .map_err(|_| "invalid_connection")
                        .and_then(|connection| self.add_connection(connection));
                    if let Err(error) = result {
                        errors.insert(error_pid, json!(error));
                    }
                }
                self.begin_request(Operation::Attach, json!({}), reply, errors);
            }
            Operation::Enable | Operation::Disable | Operation::Status => {
                self.begin_request(message.operation, message.body, reply, BTreeMap::new());
            }
            _ => one_reply(
                reply,
                Operation::Error,
                0,
                json!({"ok": false, "error": "unsupported_operation"}),
            ),
        }
    }

    fn handle_peer(&mut self, message: Message, pid: i32) {
        let Some(peer) = self.peers.get_mut(&pid) else {
            return;
        };
        if !peer.trust.accepts(pid, message.audit) || !message.body.is_object() {
            return;
        }
        if message.operation == Operation::Hello {
            if !peer.trust.hello(message.audit, &message.body) {
                self.remove_peer(pid, "injection_invalid");
            }
            return;
        }
        if !peer.trust.ready {
            return;
        }
        if message.operation == Operation::BypassLog {
            let record = LogRecord {
                kind: "record",
                helper_session: self.session.clone(),
                source_pid: pid,
                source_uid: message.audit.euid(),
                source_pid_version: message.audit.pid_version(),
                source_sequence: message.request,
                record: message.body,
            };
            if self.ring.append(&record).is_none() {
                self.oversized = self.oversized.saturating_add(1);
            }
        } else if let Some(request) = self.requests.get_mut(&message.request) {
            request
                .state
                .record(peer.key, message.operation, message.body);
        }
        // message 中未使用的 reply right 在此自动释放。
    }

    fn finish_requests(&mut self, now: Instant) {
        let completed: Vec<u64> = self
            .requests
            .iter_mut()
            .filter_map(|(id, request)| request.state.finish_if_due(now).then_some(*id))
            .collect();
        for identifier in completed {
            if let Some(request) = self.requests.remove(&identifier) {
                let live = process::live_tccd().ok();
                let connected: Vec<i32> = self.peers.keys().copied().collect();
                let (operation, body) = request.state.summary(
                    live.as_deref(),
                    &connected,
                    std::process::id(),
                    &self.session,
                    self.ring.state(self.oversized),
                );
                one_reply(request.reply, operation, identifier, body);
            }
        }
    }

    fn pump_subscribers(&mut self) {
        let names: Vec<u32> = self.subscribers.keys().copied().collect();
        for name in names {
            let mut remove = false;
            if let Some(subscriber) = self.subscribers.get_mut(&name) {
                for _ in 0..DELIVERY_BUDGET {
                    let Some(delivery) =
                        subscriber
                            .state
                            .next(&self.ring, &self.session, self.oversized)
                    else {
                        break;
                    };
                    match subscriber.send.send(
                        delivery.operation,
                        delivery.request,
                        &delivery.body,
                        ReplyPort::None,
                        0,
                    ) {
                        Ok(()) => {
                            if subscriber.state.delivered(delivery.advance) {
                                remove = true;
                                break;
                            }
                        }
                        Err(error) => {
                            if mach::is_invalid_destination(&error) {
                                remove = true;
                            }
                            // 队列满、超时和其他错误都不推进游标。
                            break;
                        }
                    }
                }
            }
            if remove {
                self.subscribers.remove(&name);
            }
        }
    }

    fn event_loop(&mut self) -> Result<()> {
        while !self.peers.is_empty() {
            let timeout = if self.subscribers.is_empty() { 100 } else { 10 };
            match mach::receive(self.port_set.name(), timeout)? {
                Event::DeadName(name) => self.handle_death(name),
                Event::Message(message) => {
                    if message.local_port == self.clients.name() {
                        self.handle_client(message);
                    } else if let Some(pid) = self.receivers.get(&message.local_port).copied() {
                        self.handle_peer(message, pid);
                    }
                }
                Event::Timeout | Event::Invalid => {}
            }
            let now = Instant::now();
            let expired: Vec<i32> = self
                .peers
                .values()
                .filter(|peer| peer.trust.handshake_expired(peer.hello_deadline, now))
                .map(|peer| peer.key.pid)
                .collect();
            for pid in expired {
                self.remove_peer(pid, "handshake_timeout");
            }
            self.finish_requests(now);
            self.pump_subscribers();
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        for (identifier, request) in std::mem::take(&mut self.requests) {
            one_reply(
                request.reply,
                Operation::Error,
                identifier,
                json!({"ok": false, "error": "all_tccd_disconnected"}),
            );
        }
        for (_, subscriber) in std::mem::take(&mut self.subscribers) {
            end_stream(&subscriber.send, "reason", "all_tccd_disconnected");
        }
        let pids: Vec<i32> = self.peers.keys().copied().collect();
        for pid in pids {
            self.remove_peer(pid, "helper_stopped");
        }
    }
}

/// 注册服务并运行。离线测试不会调用此函数。
pub fn run(connections: Vec<Connection>, capacity: usize) -> Result<()> {
    if !(1..=MAX_CAPACITY).contains(&capacity) {
        bail!("capacity must be in 1..={MAX_CAPACITY}");
    }
    process::require_root()?;
    let mut broker = Broker::new(capacity)?;
    for connection in connections {
        let pid = connection.pid;
        if let Err(error) = broker.add_connection(connection) {
            crate::lifecycle::log(&format!(
                "initial connection failed pid={pid} error={error}"
            ));
        }
    }
    if !broker.had_peers {
        bail!("no_tccd_connections");
    }
    crate::lifecycle::log("broker ready");
    let result = broker.event_loop();
    // receive 失败同样走退出路径，不能遗留订阅和单次回复。
    broker.shutdown();
    crate::lifecycle::log("broker stopped");
    result
}

fn response_body(operation: Operation, body: Value) -> (Operation, Value) {
    if serde_json::to_vec(&body).is_ok_and(|data| data.len() <= MAX_BODY) {
        (operation, body)
    } else {
        (
            Operation::Error,
            json!({"ok": false, "error": "response_too_large"}),
        )
    }
}

fn one_reply(reply: SendOnceRight, operation: Operation, identifier: u64, body: Value) {
    let (operation, body) = response_body(operation, body);
    let _ = reply.send(operation, identifier, &body, SEND_TIMEOUT_MS);
}

fn end_stream(send: &SendRight, field: &str, reason: &str) {
    let mut body = json!({"type": "end"});
    body[field] = json!(reason);
    let _ = send.send(Operation::StreamEnd, 0, &body, ReplyPort::None, 0);
}

// 保留 Foundation 的 boolValue 对布尔值、数值和字符串的处理。
fn bool_value(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => {
            let value = value.trim_start();
            if matches!(value.as_bytes().first(), Some(b'Y' | b'y' | b'T' | b't')) {
                return true;
            }
            let value = value.strip_prefix(['+', '-']).unwrap_or(value);
            value
                .bytes()
                .take_while(u8::is_ascii_digit)
                .any(|digit| digit != b'0')
        }
        _ => false,
    }
}

fn new_session() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .context("session_entropy_unavailable")?
        .read_exact(&mut bytes)
        .context("session_entropy_failed")?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pid: i32) -> PeerKey {
        PeerKey { pid, generation: 1 }
    }

    fn request(peers: &[i32]) -> RequestState {
        RequestState::new(
            12,
            Operation::Status,
            peers.iter().copied().map(key).collect(),
            Instant::now(),
            BTreeMap::new(),
        )
    }

    fn status(pid: i32, enabled: bool) -> Value {
        json!({"pid": pid, "hooks_ready": true, "enabled": enabled})
    }

    fn record(ring: &mut LogRing, n: u64) {
        ring.append(&LogRecord {
            kind: "record",
            helper_session: "session".into(),
            source_pid: 42,
            source_uid: 0,
            source_pid_version: 7,
            source_sequence: n,
            record: json!({"n": n}),
        })
        .unwrap();
    }

    fn next(state: &SubscriptionState, ring: &LogRing) -> Delivery {
        state.next(ring, "session", 0).unwrap()
    }

    #[test]
    fn timeout_only_fills_unanswered_peers() {
        let mut request = request(&[1, 2, 3]);
        request.record(key(1), Operation::Status, status(1, true));
        request.errors.insert(2, json!("send_failed"));
        assert!(!request.finish_if_due(request.deadline - Duration::from_millis(1)));
        assert!(request.finish_if_due(request.deadline));
        assert_eq!(request.errors[&2], "send_failed");
        assert_eq!(request.errors[&3], "timeout");
        assert_eq!(request.results.len(), 1);
    }

    #[test]
    fn completion_does_not_wait_for_deadline() {
        let mut request = request(&[1]);
        request.record(key(1), Operation::Status, status(1, false));
        assert!(request.finish_if_due(request.deadline - REQUEST_TIMEOUT));
        assert!(request.errors.is_empty());
    }

    #[test]
    fn reconnect_and_wrong_operation_cannot_acknowledge_request() {
        let mut request = request(&[1]);
        request.record(
            PeerKey {
                pid: 1,
                generation: 2,
            },
            Operation::Status,
            status(1, true),
        );
        request.record(key(1), Operation::Enable, status(1, true));
        request.record(key(2), Operation::Status, status(2, true));
        assert!(request.results.is_empty());
        request.disconnected(key(1), "port_dead");
        assert_eq!(request.errors[&1], "port_dead");
    }

    #[test]
    fn disconnection_does_not_erase_acknowledged_state() {
        let mut request = request(&[1, 2]);
        request.record(key(1), Operation::Status, status(1, true));
        request.disconnected(key(1), "port_dead");
        request.disconnected(
            PeerKey {
                pid: 2,
                generation: 2,
            },
            "port_dead",
        );
        assert!(request.errors.is_empty());
        request.disconnected(key(2), "handshake_timeout");
        assert_eq!(request.errors[&2], "handshake_timeout");
    }

    #[test]
    fn attach_uses_status_but_replies_with_attach() {
        let mut request = request(&[1]);
        request.operation = Operation::Attach;
        assert_eq!(request.wire_operation(), Operation::Status);
        request.record(key(1), Operation::Status, status(1, true));
        let (operation, body) = request.summary(Some(&[1]), &[1], 99, "session", json!({}));
        assert_eq!(operation, Operation::Attach);
        assert_eq!(body["ok"], true);
        assert_eq!(body["request_id"], 12);
    }

    #[test]
    fn status_requires_all_live_processes_and_all_acknowledgments() {
        let mut request = request(&[1, 2]);
        request.record(key(1), Operation::Status, status(1, true));
        request.record(key(2), Operation::Status, status(2, false));
        let (operation, body) = request.summary(Some(&[1, 2]), &[1, 2], 99, "session", json!({}));
        assert_eq!(operation, Operation::Status);
        assert_eq!(body["ok"], true);
        assert_eq!(body["enabled"], false);
        assert_eq!(body["acknowledged"], 2);
        let (operation, body) =
            request.summary(Some(&[1, 2, 3]), &[1, 2], 99, "session", json!({}));
        assert_eq!(operation, Operation::Error);
        assert_eq!(body["missing_pids"], json!([3]));
        assert_eq!(body["error"], "incomplete_or_stale_connections");
    }

    #[test]
    fn stale_equal_count_and_discovery_failure_are_not_success() {
        let mut request = request(&[1]);
        request.record(key(1), Operation::Status, status(1, true));
        for (live, connected) in [
            (Some(&[2][..]), &[2][..]),
            (Some(&[][..]), &[][..]),
            (None, &[1][..]),
        ] {
            let (operation, body) = request.summary(live, connected, 99, "session", json!({}));
            assert_eq!(operation, Operation::Error);
            assert_eq!(body["ok"], false);
            assert_eq!(body["enabled"], false);
            if live.is_none() {
                assert_eq!(body["error"], "process_discovery_failed");
            }
        }
    }

    #[test]
    fn target_errors_and_invalid_hooks_fail_aggregation() {
        let mut request = request(&[1, 2]);
        request.record(key(1), Operation::Error, json!({}));
        request.record(key(2), Operation::Status, json!({"hooks_ready": false}));
        assert!(request.complete());
        assert_eq!(request.errors[&1], "target_error");
        assert_eq!(request.errors[&2], "injection_invalid");
        let (_, body) = request.summary(Some(&[1, 2]), &[1, 2], 99, "session", json!({}));
        assert_eq!(body["errors"].as_array().unwrap().len(), 2);
        assert_eq!(body["acknowledged"], 0);
    }

    #[test]
    fn no_peers_and_attach_errors_cannot_report_success() {
        let mut request = request(&[]);
        assert!(request.complete());
        request.errors.insert(7, json!("invalid_connection"));
        let (_, body) = request.summary(Some(&[]), &[], 99, "session", json!({}));
        assert_eq!(body["ok"], false);
        assert_eq!(
            body["errors"],
            json!([{"pid": 7, "error": "invalid_connection"}])
        );
    }

    #[test]
    fn oversized_summary_is_replaced_with_bounded_error() {
        let (operation, body) =
            response_body(Operation::Status, json!({"large": "x".repeat(MAX_BODY)}));
        assert_eq!(operation, Operation::Error);
        assert_eq!(body["error"], "response_too_large");
        let (operation, body) = response_body(Operation::Disable, json!({"ok": true}));
        assert_eq!(operation, Operation::Disable);
        assert_eq!(body["ok"], true);
    }

    #[test]
    fn empty_snapshot_has_begin_then_end() {
        let ring = LogRing::new(2, BYTE_LIMIT);
        let mut state = SubscriptionState::new(&ring, true);
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::ReplayBegin);
        assert_eq!(delivery.body["first_sequence"], 1);
        assert_eq!(delivery.body["snapshot_end"], 0);
        assert!(!state.delivered(delivery.advance));
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::ReplayEnd);
        assert!(state.delivered(delivery.advance));
    }

    #[test]
    fn snapshot_boundary_excludes_new_records() {
        let mut ring = LogRing::new(10, BYTE_LIMIT);
        record(&mut ring, 1);
        let mut state = SubscriptionState::new(&ring, true);
        record(&mut ring, 2);
        state.delivered(next(&state, &ring).advance);
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::BypassLog);
        assert_eq!(delivery.request, 1);
        state.delivered(delivery.advance);
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::ReplayEnd);
        assert_eq!(delivery.body["snapshot_end"], 1);
        assert!(state.delivered(delivery.advance));
    }

    #[test]
    fn follow_delivers_records_after_replay_end() {
        let mut ring = LogRing::new(10, BYTE_LIMIT);
        let mut state = SubscriptionState::new(&ring, false);
        state.delivered(next(&state, &ring).advance);
        assert!(!state.delivered(next(&state, &ring).advance));
        assert!(state.next(&ring, "session", 0).is_none());
        record(&mut ring, 1);
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::BypassLog);
        assert_eq!(delivery.request, 1);
        state.delivered(delivery.advance);
        assert!(state.next(&ring, "session", 0).is_none());
    }

    #[test]
    fn eviction_gap_is_clamped_to_replay_boundary() {
        let mut ring = LogRing::new(1, BYTE_LIMIT);
        record(&mut ring, 1);
        let mut state = SubscriptionState::new(&ring, false);
        state.delivered(next(&state, &ring).advance);
        for n in 2..=4 {
            record(&mut ring, n);
        }
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::Gap);
        assert_eq!(delivery.body["from"], 1);
        assert_eq!(delivery.body["to"], 1);
        state.delivered(delivery.advance);
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::ReplayEnd);
        assert!(!state.delivered(delivery.advance));
        let delivery = next(&state, &ring);
        assert_eq!(delivery.operation, Operation::Gap);
        assert_eq!(delivery.body["from"], 2);
        assert_eq!(delivery.body["to"], 3);
        state.delivered(delivery.advance);
        assert_eq!(next(&state, &ring).request, 4);
    }

    #[test]
    fn blocked_delivery_keeps_cursor_and_frame() {
        let mut ring = LogRing::new(2, BYTE_LIMIT);
        record(&mut ring, 1);
        let mut state = SubscriptionState::new(&ring, false);
        let first = next(&state, &ring);
        let retry = next(&state, &ring);
        assert_eq!(first.advance, retry.advance);
        assert_eq!(first.body, retry.body);
        assert!(!state.began);
        state.delivered(first.advance);
        let first = next(&state, &ring);
        let retry = next(&state, &ring);
        assert_eq!(first.operation, Operation::BypassLog);
        assert_eq!(first.request, retry.request);
        assert_eq!(state.cursor, 1);
        state.delivered(first.advance);
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn delivery_budget_caps_per_iteration_work() {
        let mut ring = LogRing::new(20, BYTE_LIMIT);
        for n in 1..=20 {
            record(&mut ring, n);
        }
        let mut state = SubscriptionState::new(&ring, false);
        for _ in 0..DELIVERY_BUDGET {
            state.delivered(next(&state, &ring).advance);
        }
        // 一帧 begin 加七条记录。
        assert_eq!(state.cursor, 8);
        assert!(!state.replay_ended);
        assert_eq!(REQUEST_LIMIT, 64);
        assert_eq!(SUBSCRIBER_LIMIT, 16);
    }

    #[test]
    fn handshake_pins_every_audit_token_word() {
        let token = AuditToken {
            val: [10, 0, 12, 13, 14, 42, 16, 17],
        };
        let mut trust = PeerTrust::default();
        assert!(trust.accepts(42, token));
        assert!(!trust.accepts(43, token));
        assert!(!trust.hello(token, &json!({"hooks_ready": false})));
        assert!(!trust.ready);
        assert!(trust.audit.is_none());
        assert!(trust.hello(token, &json!({"hooks_ready": true})));
        assert!(trust.ready);
        assert!(trust.accepts(42, token));
        for index in 0..8 {
            let mut changed = token;
            changed.val[index] ^= 1;
            assert!(!trust.accepts(42, changed), "audit word {index}");
        }
    }

    #[test]
    fn handshake_deadline_applies_only_before_ready() {
        let now = Instant::now();
        let deadline = now + HELLO_TIMEOUT;
        let mut trust = PeerTrust::default();
        assert!(!trust.handshake_expired(deadline, now));
        assert!(trust.handshake_expired(deadline, deadline));
        trust.ready = true;
        assert!(!trust.handshake_expired(deadline, deadline + Duration::from_secs(10)));
    }

    #[test]
    fn admission_limits_do_not_allow_one_extra_request_or_subscriber() {
        assert!(request_available(0, 1));
        assert!(request_available(63, u64::MAX - 1));
        assert!(!request_available(64, 1));
        assert!(!request_available(0, u64::MAX));
        assert!(subscriber_available(15));
        assert!(!subscriber_available(16));
    }

    #[test]
    fn foundation_boolean_compatibility() {
        for value in [
            json!(true),
            json!(1),
            json!(-1),
            json!("YES"),
            json!(" true"),
            json!("+12suffix"),
        ] {
            assert!(bool_value(&value));
        }
        for value in [
            Value::Null,
            json!(false),
            json!(0),
            json!("NO"),
            json!("false"),
            json!([]),
            json!("000"),
        ] {
            assert!(!bool_value(&value));
        }
    }
}
