//! 单线程日志环。保留条目数和编码字节数两种限制。

use crate::protocol::MAX_BODY;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;

pub const MAX_CAPACITY: usize = 65_536;
pub const BYTE_LIMIT: usize = 8 * 1024 * 1024;

/// 来源字段由 broker 从 audit token 取得，不信任日志载荷。
#[derive(Debug, Clone, Serialize)]
pub struct LogRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub helper_session: String,
    pub source_pid: i32,
    pub source_uid: u32,
    pub source_pid_version: u32,
    pub source_sequence: u64,
    pub record: Value,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub sequence: u64,
    pub body: Value,
    encoded_bytes: usize,
}

#[derive(Debug)]
pub struct LogRing {
    entries: VecDeque<LogEntry>,
    capacity: usize,
    byte_limit: usize,
    bytes: usize,
    next: u64,
    evicted: u64,
}

impl LogRing {
    /// 调用端校验容量。零容量仍可用于离线测试。
    pub fn new(capacity: usize, byte_limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.min(MAX_CAPACITY),
            byte_limit: byte_limit.min(BYTE_LIMIT),
            bytes: 0,
            next: 1,
            evicted: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn first_sequence(&self) -> u64 {
        self.entries
            .front()
            .map_or(self.next, |entry| entry.sequence)
    }

    pub fn last_sequence(&self) -> u64 {
        self.next - 1
    }

    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    pub fn state(&self, oversized: u64) -> Value {
        json!({
            "records": self.count(),
            "bytes": self.bytes(),
            "first_sequence": self.first_sequence(),
            "last_sequence": self.last_sequence(),
            "evicted": self.evicted(),
            "oversized": oversized,
        })
    }

    /// 拒绝的条目不占用序号，也不驱逐已有条目。
    pub fn append(&mut self, record: &LogRecord) -> Option<&LogEntry> {
        self.append_value(serde_json::to_value(record).ok()?)
    }

    fn append_value(&mut self, mut record: Value) -> Option<&LogEntry> {
        record
            .as_object_mut()?
            .insert("sequence".into(), json!(self.next));
        let size = serde_json::to_vec(&record).ok()?.len();
        let next = self.next.checked_add(1)?;
        if self.capacity == 0 || size > MAX_BODY || size > self.byte_limit {
            return None;
        }
        while !self.entries.is_empty()
            && (self.entries.len() == self.capacity || self.bytes + size > self.byte_limit)
        {
            let removed = self.entries.pop_front()?;
            self.bytes -= removed.encoded_bytes;
            self.evicted = self.evicted.saturating_add(1);
        }
        self.entries.push_back(LogEntry {
            sequence: self.next,
            body: record,
            encoded_bytes: size,
        });
        self.next = next;
        self.bytes += size;
        self.entries.back()
    }

    pub fn entry(&self, sequence: u64) -> Option<&LogEntry> {
        if self.entries.is_empty() || sequence < self.first_sequence() || sequence >= self.next {
            return None;
        }
        let offset = usize::try_from(sequence - self.first_sequence()).ok()?;
        self.entries.get(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ring_has_next_as_first_sequence() {
        let ring = LogRing::new(2, BYTE_LIMIT);
        assert_eq!(ring.count(), 0);
        assert_eq!(ring.bytes(), 0);
        assert_eq!(ring.first_sequence(), 1);
        assert_eq!(ring.last_sequence(), 0);
        assert!(ring.entry(0).is_none());
        assert!(ring.entry(1).is_none());
    }

    #[test]
    fn capacity_eviction_preserves_contiguous_sequence() {
        let mut ring = LogRing::new(2, BYTE_LIMIT);
        for i in 0..7 {
            assert!(ring.append_value(json!({"n": i})).is_some());
        }
        assert_eq!(ring.count(), 2);
        assert_eq!(ring.evicted(), 5);
        assert_eq!(ring.first_sequence(), 6);
        assert_eq!(ring.last_sequence(), 7);
        assert!(ring.entry(5).is_none());
        assert_eq!(ring.entry(6).unwrap().body["n"], 5);
        assert_eq!(ring.entry(7).unwrap().body["sequence"], 7);
        assert!(ring.entry(8).is_none());
    }

    #[test]
    fn byte_limit_evicts_independently_of_capacity() {
        let record = json!({"payload": "abcdefgh", "sequence": 1});
        let size = serde_json::to_vec(&record).unwrap().len();
        let mut ring = LogRing::new(10, size * 2);
        for _ in 0..3 {
            ring.append_value(record.clone()).unwrap();
        }
        assert_eq!(ring.count(), 2);
        assert_eq!(ring.bytes(), size * 2);
        assert_eq!(ring.first_sequence(), 2);
        assert_eq!(ring.evicted(), 1);
    }

    #[test]
    fn oversized_record_does_not_evict_or_consume_sequence() {
        let mut ring = LogRing::new(1, 100);
        ring.append_value(json!({"small": true})).unwrap();
        let before = ring.state(0);
        assert!(ring.append_value(json!({"big": "x".repeat(101)})).is_none());
        assert_eq!(ring.state(0), before);
        assert_eq!(
            ring.append_value(json!({"small": false})).unwrap().sequence,
            2
        );
    }

    #[test]
    fn maximum_wire_body_includes_envelope_and_sequence() {
        let mut ring = LogRing::new(10, BYTE_LIMIT);
        let empty_size = serde_json::to_vec(&json!({"x": "", "sequence": 1}))
            .unwrap()
            .len();
        ring.append_value(json!({"x": "x".repeat(MAX_BODY - empty_size)}))
            .unwrap();
        assert_eq!(ring.bytes(), MAX_BODY);
        assert!(ring
            .append_value(json!({"x": "x".repeat(MAX_BODY)}))
            .is_none());
        assert_eq!(ring.count(), 1);
    }

    #[test]
    fn zero_limits_and_non_objects_reject_without_mutation() {
        for (capacity, bytes) in [(0, BYTE_LIMIT), (10, 0)] {
            let mut ring = LogRing::new(capacity, bytes);
            assert!(ring.append_value(json!({})).is_none());
            assert!(ring.append_value(json!([])).is_none());
            assert_eq!(ring.first_sequence(), 1);
            assert_eq!(ring.last_sequence(), 0);
        }
    }

    #[test]
    fn authenticated_envelope_is_preserved() {
        let mut ring = LogRing::new(10, BYTE_LIMIT);
        let entry = ring
            .append(&LogRecord {
                kind: "record",
                helper_session: "session".into(),
                source_pid: 100,
                source_uid: 0,
                source_pid_version: 9,
                source_sequence: 52,
                record: json!({"source_pid": 999, "sequence": 999}),
            })
            .unwrap();
        assert_eq!(entry.body["type"], "record");
        assert_eq!(entry.body["source_pid"], 100);
        assert_eq!(entry.body["source_pid_version"], 9);
        assert_eq!(entry.body["source_sequence"], 52);
        assert_eq!(entry.body["sequence"], 1);
        assert_eq!(entry.body["record"]["source_pid"], 999);
    }

    #[test]
    fn sequence_exhaustion_rejects_without_wrapping() {
        let mut ring = LogRing::new(10, BYTE_LIMIT);
        ring.next = u64::MAX;
        assert!(ring.append_value(json!({})).is_none());
        assert_eq!(ring.next, u64::MAX);
        assert_eq!(ring.count(), 0);
    }
}
