//! 控制协议与上游 `native/ControlProtocol.h` 共用 wire ABI。
//! Mach 端口名称只在拥有它的 task 中有效。

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SERVICE: &str = "dev.xtcc.control";
pub const MAX_BODY: usize = 16_384;
pub(crate) const MAGIC: u32 = 0x5443_4342;
pub(crate) const VERSION: u32 = 1;
pub(crate) const BODY_OFFSET: usize = 48;
pub(crate) const FRAME_CAPACITY: usize = BODY_OFFSET + MAX_BODY;
pub(crate) const COMPLEX: u32 = 0x8000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    Hello = 1,
    Enable = 2,
    Disable = 3,
    Status = 4,
    BypassLog = 5,
    Attach = 6,
    Subscribe = 7,
    ReplayBegin = 8,
    ReplayEnd = 9,
    StreamEnd = 10,
    Gap = 11,
    Error = 255,
}

impl TryFrom<u32> for Operation {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self> {
        Ok(match value {
            1 => Self::Hello,
            2 => Self::Enable,
            3 => Self::Disable,
            4 => Self::Status,
            5 => Self::BypassLog,
            6 => Self::Attach,
            7 => Self::Subscribe,
            8 => Self::ReplayBegin,
            9 => Self::ReplayEnd,
            10 => Self::StreamEnd,
            11 => Self::Gap,
            255 => Self::Error,
            _ => bail!("unknown operation: {value}"),
        })
    }
}

/// 保留全部八个字段；使用 SDK 的 libbsm 解释身份字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct AuditToken {
    pub val: [u32; 8],
}

#[link(name = "bsm")]
extern "C" {
    fn audit_token_to_pid(token: AuditToken) -> i32;
    fn audit_token_to_euid(token: AuditToken) -> u32;
    fn audit_token_to_pidversion(token: AuditToken) -> i32;
}

impl AuditToken {
    pub fn pid(self) -> i32 {
        // SAFETY: AuditToken 与 SDK 的 audit_token_t 布局一致，参数按值传入。
        unsafe { audit_token_to_pid(self) }
    }

    pub fn euid(self) -> u32 {
        // SAFETY: 此 token 完整包含 SDK 要求的八个 u32 字段。
        unsafe { audit_token_to_euid(self) }
    }

    pub fn pid_version(self) -> u32 {
        // SAFETY: ABI 同 audit_token_t。转换保留 pidversion 的全部位。
        unsafe { audit_token_to_pidversion(self) as u32 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub pid: i32,
    pub receive: u32,
}

/// SDK 的用户态 mach_msg_header_t：24 字节，四字节对齐。
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub(crate) struct Header {
    pub bits: u32,
    pub size: u32,
    pub remote_port: u32,
    pub local_port: u32,
    pub voucher_port: u32,
    pub id: i32,
}

/// 不使用 Rust 默认的 u64 对齐规则。
#[repr(C, packed(4))]
pub(crate) struct Frame {
    pub header: Header,
    pub magic: u32,
    pub version: u32,
    pub operation: u32,
    pub length: u32,
    pub request: u64,
    pub body: [u8; MAX_BODY],
}

pub(crate) fn frame_size(length: usize) -> Result<usize> {
    ensure!(length <= MAX_BODY, "body exceeds {MAX_BODY} bytes");
    Ok((BODY_OFFSET + length + 3) & !3)
}

impl Frame {
    pub(crate) fn json(operation: Operation, request: u64, body: &Value) -> Result<Self> {
        let encoded = serde_json::to_vec(body).context("encode JSON body")?;
        Self::bytes(operation, request, &encoded)
    }

    pub(crate) fn bytes(operation: Operation, request: u64, body: &[u8]) -> Result<Self> {
        let size = frame_size(body.len())?;
        let mut frame = Self {
            header: Header {
                size: size as u32,
                id: MAGIC as i32,
                ..Header::default()
            },
            magic: MAGIC,
            version: VERSION,
            operation: operation as u32,
            length: body.len() as u32,
            request,
            body: [0; MAX_BODY],
        };
        frame.body[..body.len()].copy_from_slice(body);
        Ok(frame)
    }
}

pub(crate) struct Decoded {
    pub operation: Operation,
    pub request: u64,
    pub body: Value,
}

pub(crate) fn word(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset.checked_add(4).context("word offset overflow")?;
    let word: [u8; 4] = bytes
        .get(offset..end)
        .context("truncated message word")?
        .try_into()
        .expect("four-byte slice");
    Ok(u32::from_ne_bytes(word))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded> {
    ensure!(bytes.len() >= BODY_OFFSET, "truncated frame");
    let size = word(bytes, 4)? as usize;
    ensure!(word(bytes, 0)? & COMPLEX == 0, "complex protocol frame");
    ensure!(word(bytes, 20)? == MAGIC, "invalid message id");
    ensure!(word(bytes, 24)? == MAGIC, "invalid protocol magic");
    ensure!(word(bytes, 28)? == VERSION, "unsupported protocol version");
    let operation = Operation::try_from(word(bytes, 32)?)?;
    let length = word(bytes, 36)? as usize;
    ensure!(size == frame_size(length)?, "invalid frame size");
    ensure!(bytes.len() >= size, "truncated message body");
    let request = u64::from_ne_bytes(bytes[40..48].try_into().expect("eight-byte slice"));
    let body = &bytes[BODY_OFFSET..BODY_OFFSET + length];
    // 上游允许 Hello 的空 body；这是唯一不要求 JSON 的操作。
    let body = if body.is_empty() && operation == Operation::Hello {
        Value::Object(Default::default())
    } else {
        serde_json::from_slice(body).context("invalid JSON body")?
    };
    Ok(Decoded {
        operation,
        request,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    fn wire(frame: &Frame) -> Vec<u8> {
        // SAFETY: Frame 所有字节均已初始化；pack(4) 布局不存在未初始化 padding。
        unsafe {
            std::slice::from_raw_parts((frame as *const Frame).cast::<u8>(), size_of::<Frame>())
                .to_vec()
        }
    }

    fn replace(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    }

    #[test]
    fn sdk_pack_four_layout() {
        // Xcode 27 SDK 上的小 C 程序验证了这些 sizeof/offsetof。
        assert_eq!(size_of::<Header>(), 24);
        assert_eq!(align_of::<Header>(), 4);
        assert_eq!(align_of::<Frame>(), 4);
        assert_eq!(size_of::<Frame>(), 16_432);
        assert_eq!(offset_of!(Frame, magic), 24);
        assert_eq!(offset_of!(Frame, request), 40);
        assert_eq!(offset_of!(Frame, body), 48);
        assert_eq!(size_of::<AuditToken>(), 32);
        assert_eq!(align_of::<AuditToken>(), 4);
    }

    #[test]
    fn operations_and_audit_fields() {
        for number in 1..=11 {
            assert_eq!(Operation::try_from(number).unwrap() as u32, number);
        }
        assert_eq!(Operation::try_from(255).unwrap(), Operation::Error);
        for number in [0, 12, 254, 256, u32::MAX] {
            assert!(Operation::try_from(number).is_err());
        }
        let token = AuditToken {
            val: [10, 11, 12, 13, 14, 15, 16, u32::MAX],
        };
        assert_eq!(token.euid(), 11);
        assert_eq!(token.pid(), 15);
        assert_eq!(token.pid_version(), u32::MAX);
        assert_ne!(token, AuditToken { val: [0; 8] });
    }

    #[test]
    fn every_body_length_is_four_byte_aligned() {
        for length in 0..=MAX_BODY {
            let size = frame_size(length).unwrap();
            assert_eq!(size % 4, 0);
            assert!(size >= BODY_OFFSET + length);
            assert!(size < BODY_OFFSET + length + 4);
        }
        assert!(frame_size(MAX_BODY + 1).is_err());
        assert!(frame_size(usize::MAX).is_err());
    }

    #[test]
    fn maximum_json_body_and_request_round_trip() {
        let body = Value::String("x".repeat(MAX_BODY - 2));
        let frame = Frame::json(Operation::Status, u64::MAX, &body).unwrap();
        let decoded = decode(&wire(&frame)).unwrap();
        assert_eq!(decoded.operation, Operation::Status);
        assert_eq!(decoded.request, u64::MAX);
        assert_eq!(decoded.body, body);
        assert!(Frame::json(
            Operation::Status,
            1,
            &Value::String("x".repeat(MAX_BODY - 1))
        )
        .is_err());
    }

    #[test]
    fn hello_empty_body_and_zero_padding() {
        let hello = Frame::bytes(Operation::Hello, 0, &[]).unwrap();
        assert_eq!(decode(&wire(&hello)).unwrap().body, serde_json::json!({}));
        let status = Frame::bytes(Operation::Status, 0, &[]).unwrap();
        assert!(decode(&wire(&status)).is_err());
        let frame = Frame::bytes(Operation::Status, 0, b"{\"x\":1}").unwrap();
        let size = frame.header.size as usize;
        let bytes = wire(&frame);
        assert!(bytes[BODY_OFFSET + 7..size].iter().all(|byte| *byte == 0));
        assert!(decode(&bytes[..size - 1]).is_err());
        assert!(decode(&bytes[..BODY_OFFSET - 1]).is_err());
    }

    #[test]
    fn reject_protocol_boundaries_and_invalid_json() {
        let frame = Frame::json(Operation::Status, 123, &serde_json::json!({"ok":true})).unwrap();
        let bytes = wire(&frame);
        for (offset, value) in [
            (0, COMPLEX),
            (4, 0),
            (4, u32::MAX),
            (20, 0),
            (24, 0),
            (28, VERSION + 1),
            (32, 0),
            (36, (MAX_BODY + 1) as u32),
            (36, u32::MAX),
        ] {
            let mut invalid = bytes.clone();
            replace(&mut invalid, offset, value);
            assert!(decode(&invalid).is_err(), "offset={offset} value={value}");
        }
        for body in [&b"{"[..], &b"{} garbage"[..], &[0xff][..]] {
            let frame = Frame::bytes(Operation::Status, 0, body).unwrap();
            assert!(decode(&wire(&frame)).is_err());
        }
    }

    #[test]
    fn native_header_fixture_matches_rust_layout_and_wire() {
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct Temporary(std::path::PathBuf);
        impl Drop for Temporary {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temporary = Temporary(
            std::env::temp_dir().join(format!("xtcc-abi-{}-{nonce}", std::process::id())),
        );
        std::fs::create_dir(&temporary.0).unwrap();
        let binary = temporary.0.join("native-abi");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_abi.m");
        let compile = Command::new("/usr/bin/xcrun")
            .args([
                "clang",
                "-fobjc-arc",
                "-framework",
                "Foundation",
                "-lbsm",
                "-Wall",
                "-Wextra",
                "-Werror",
            ])
            .arg(&fixture)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary).output().unwrap();
        assert!(output.status.success());
        let native: Value = serde_json::from_slice(&output.stdout).unwrap();
        for (key, expected) in [
            ("header", size_of::<Header>()),
            ("frame", size_of::<Frame>()),
            ("align", align_of::<Frame>()),
            ("request_offset", offset_of!(Frame, request)),
            ("body_offset", BODY_OFFSET),
            ("audit_trailer", 52),
            ("audit_offset", 20),
            ("max_trailer", 68),
            ("death_size", 36),
            ("death_port_offset", 32),
        ] {
            assert_eq!(native[key].as_u64(), Some(expected as u64), "{key}");
        }
        assert_eq!(native["service"].as_str(), Some(SERVICE));
        let hex = native["wire"].as_str().unwrap();
        let native_wire: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap())
            .collect();
        let decoded = decode(&native_wire).unwrap();
        assert_eq!(decoded.operation, Operation::Status);
        assert_eq!(decoded.request, 0x1020304050607080);
        assert_eq!(decoded.body, serde_json::json!({"ok": true}));
        let frame = Frame::json(Operation::Status, decoded.request, &decoded.body).unwrap();
        let rust_wire = wire(&frame);
        assert_eq!(native_wire, &rust_wire[..frame.header.size as usize]);
    }
}
