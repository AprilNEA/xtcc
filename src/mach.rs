//! Mach rights are owned by RAII types; received messages are destroyed by the receive buffer.
//! Tests in this module use only ports created by this process, without registering services
//! or acquiring other tasks.

use crate::protocol::{self, AuditToken, Frame, Header, Operation};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::{error::Error, fmt, mem, ptr};

pub type Port = u32;

// These values were checked against mach/{message,port,notify}.h in the local Xcode 27 SDK.
const RIGHT_SEND: u32 = 0;
const RIGHT_RECEIVE: u32 = 1;
const RIGHT_PORT_SET: u32 = 3;
const TYPE_MOVE_SEND: u32 = 17;
const TYPE_MOVE_SEND_ONCE: u32 = 18;
const TYPE_MAKE_SEND: u32 = 20;
const TYPE_MAKE_SEND_ONCE: u32 = 21;
const PORT_DEAD: Port = u32::MAX;
const PORT_TYPE_DEAD_NAME: u32 = 1 << (4 + 16);
const NOTIFY_DEAD_NAME: i32 = 72;
const DEAD_NAME_MESSAGE_SIZE: usize = 36;
const SEND_MSG: i32 = 0x1;
const RCV_MSG: i32 = 0x2;
const SEND_TIMEOUT: i32 = 0x10;
const RCV_TIMEOUT: i32 = 0x100;
const RCV_TRAILER_AUDIT: i32 = 3 << 24;
const SEND_INVALID_DEST: i32 = 0x1000_0003;
const SEND_TIMED_OUT: i32 = 0x1000_0004;
const RCV_TIMED_OUT: i32 = 0x1000_4003;
const RCV_TOO_LARGE: i32 = 0x1000_4004;
const RCV_HEADER_ERROR: i32 = 0x1000_400b;
const RCV_BODY_ERROR: i32 = 0x1000_400c;
const MSG_ERROR_MASK: i32 = 0x3e00;
const REMOTE_MASK: u32 = 0x1f;
const LOCAL_MASK: u32 = 0x1f00;
const VOUCHER_MASK: u32 = 0x1f0000;
const AUDIT_TRAILER_SIZE: usize = 52;
const MAX_TRAILER_SIZE: usize = 68;
const RECEIVE_CAPACITY: usize = protocol::FRAME_CAPACITY + MAX_TRAILER_SIZE;

#[link(name = "System")]
extern "C" {
    fn mach_port_allocate(task: Port, right: u32, name: *mut Port) -> i32;
    fn mach_port_deallocate(task: Port, name: Port) -> i32;
    fn mach_port_mod_refs(task: Port, name: Port, right: u32, delta: i32) -> i32;
    fn mach_port_insert_right(task: Port, name: Port, poly: Port, disposition: u32) -> i32;
    fn mach_port_extract_right(
        task: Port,
        name: Port,
        disposition: u32,
        poly: *mut Port,
        kind: *mut u32,
    ) -> i32;
    fn mach_port_move_member(task: Port, member: Port, set: Port) -> i32;
    fn mach_port_set_attributes(
        task: Port,
        name: Port,
        flavor: i32,
        info: *mut i32,
        count: u32,
    ) -> i32;
    fn mach_port_request_notification(
        task: Port,
        name: Port,
        id: i32,
        sync: u32,
        notify: Port,
        disposition: u32,
        previous: *mut Port,
    ) -> i32;
    fn mach_port_type(task: Port, name: Port, kind: *mut u32) -> i32;
    fn task_for_pid(task: Port, pid: i32, target: *mut Port) -> i32;
    fn mach_msg(
        message: *mut Header,
        options: i32,
        send_size: u32,
        receive_size: u32,
        receive_name: Port,
        timeout: u32,
        notify: Port,
    ) -> i32;
    fn mach_msg_destroy(message: *mut Header);
    static mut mach_task_self_: Port;
    static mut bootstrap_port: Port;
    fn bootstrap_look_up(bootstrap: Port, service: *const libc::c_char, send: *mut Port) -> i32;
    fn bootstrap_register(bootstrap: Port, service: *const libc::c_char, send: Port) -> i32;
}

fn task_self() -> Port {
    // SAFETY: libSystem sets this task name during process initialization; this program never modifies it.
    unsafe { mach_task_self_ }
}

fn valid(name: Port) -> bool {
    name != 0 && name != PORT_DEAD
}

#[derive(Debug)]
struct MachFailure {
    operation: &'static str,
    code: i32,
}

impl fmt::Display for MachFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {:#010x}", self.operation, self.code)
    }
}
impl Error for MachFailure {}

fn check(code: i32, operation: &'static str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(MachFailure { operation, code }.into())
    }
}

pub fn is_invalid_destination(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<MachFailure>()
        .is_some_and(|error| error.code == SEND_INVALID_DEST)
}

pub fn is_timeout(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<MachFailure>()
        .is_some_and(|error| matches!(error.code, SEND_TIMED_OUT | RCV_TIMED_OUT))
}

fn release_send(name: Port) {
    if valid(name) {
        // SAFETY: The caller owns a send, send-once, or converted dead-name reference.
        // Drop does not report errors; port death does not change mach_port_deallocate's reference-release rules.
        unsafe {
            mach_port_deallocate(task_self(), name);
        }
    }
}

#[derive(Debug)]
pub struct SendRight {
    name: Port,
}
#[derive(Debug)]
pub struct SendOnceRight {
    name: Port,
}
#[derive(Debug)]
pub struct ReceiveRight {
    name: Port,
}
#[derive(Debug)]
pub struct PortSet {
    name: Port,
}

impl Drop for SendRight {
    fn drop(&mut self) {
        release_send(self.name);
    }
}
impl Drop for SendOnceRight {
    fn drop(&mut self) {
        release_send(self.name);
    }
}
impl Drop for ReceiveRight {
    fn drop(&mut self) {
        if valid(self.name) {
            // SAFETY: This object exclusively owns the receive right; releasing it does not destroy send references with the same name.
            unsafe {
                mach_port_mod_refs(task_self(), self.name, RIGHT_RECEIVE, -1);
            }
        }
    }
}
impl Drop for PortSet {
    fn drop(&mut self) {
        if valid(self.name) {
            // SAFETY: This object owns the port-set right; the set does not own its members' receive rights.
            unsafe {
                mach_port_mod_refs(task_self(), self.name, RIGHT_PORT_SET, -1);
            }
        }
    }
}

fn allocate(right: u32) -> Result<Port> {
    let mut name = 0;
    // SAFETY: name points to a valid, writable mach_port_name_t.
    check(
        unsafe { mach_port_allocate(task_self(), right, &mut name) },
        "mach_port_allocate",
    )?;
    ensure!(valid(name), "mach_port_allocate returned invalid name");
    Ok(name)
}

impl ReceiveRight {
    pub fn name(&self) -> Port {
        self.name
    }

    pub fn new() -> Result<Self> {
        let receive = Self {
            name: allocate(RIGHT_RECEIVE)?,
        };
        let mut limit = 128i32;
        // SAFETY: flavor=1 requires a single-u32 mach_port_limits_t, with count=1.
        // i32 has the same size and alignment as the SDK's natural_t; 128 has the same value in both types.
        check(
            unsafe { mach_port_set_attributes(task_self(), receive.name, 1, &mut limit, 1) },
            "mach_port_set_attributes",
        )?;
        Ok(receive)
    }

    pub fn make_send(&self) -> Result<SendRight> {
        // SAFETY: Borrowing self keeps the receive right alive; MAKE_SEND creates a new send reference.
        check(
            unsafe { mach_port_insert_right(task_self(), self.name, self.name, TYPE_MAKE_SEND) },
            "mach_port_insert_right",
        )?;
        Ok(SendRight { name: self.name })
    }

    fn make_send_once(&self) -> Result<SendOnceRight> {
        let mut name = 0;
        let mut kind = 0;
        // SAFETY: self owns the receive right; both output pointers are valid. MAKE_SEND_ONCE does not move the receive right.
        check(
            unsafe {
                mach_port_extract_right(
                    task_self(),
                    self.name,
                    TYPE_MAKE_SEND_ONCE,
                    &mut name,
                    &mut kind,
                )
            },
            "make send-once right",
        )?;
        let right = SendOnceRight { name };
        ensure!(
            valid(name) && kind == TYPE_MOVE_SEND_ONCE,
            "unexpected extracted send-once type: {kind}"
        );
        Ok(right)
    }
}

impl PortSet {
    pub fn name(&self) -> Port {
        self.name
    }
    pub fn new() -> Result<Self> {
        Ok(Self {
            name: allocate(RIGHT_PORT_SET)?,
        })
    }
    pub fn add(&self, receive: &ReceiveRight) -> Result<()> {
        // SAFETY: The two objects own this task's port-set and receive rights, respectively.
        check(
            unsafe { mach_port_move_member(task_self(), receive.name, self.name) },
            "mach_port_move_member",
        )
    }
}

impl SendRight {
    pub fn name(&self) -> Port {
        self.name
    }

    fn into_raw(mut self) -> Port {
        let name = self.name;
        self.name = 0;
        name
    }

    pub fn try_clone(&self) -> Result<Self> {
        // SAFETY: +1 adds a send user reference, transferred to the returned RAII object on success.
        let code = unsafe { mach_port_mod_refs(task_self(), self.name, RIGHT_SEND, 1) };
        if code != 0 {
            let mut kind = 0;
            // SAFETY: kind is a writable output; port_type neither acquires nor releases any rights.
            let type_code = unsafe { mach_port_type(task_self(), self.name, &mut kind) };
            if type_code == 0 && kind & PORT_TYPE_DEAD_NAME != 0 {
                return Err(MachFailure {
                    operation: "clone dead destination",
                    code: SEND_INVALID_DEST,
                }
                .into());
            }
        }
        check(code, "mach_port_mod_refs send +1")?;
        Ok(Self { name: self.name })
    }

    pub fn watch_dead(&self, notifications: &ReceiveRight) -> Result<()> {
        self.notification(notifications.name, TYPE_MAKE_SEND_ONCE)
    }

    pub fn unwatch_dead(&self) -> Result<()> {
        // MIG still requires a valid send-once disposition even when notify is MACH_PORT_NULL.
        self.notification(0, TYPE_MAKE_SEND_ONCE)
    }

    fn notification(&self, notify: Port, disposition: u32) -> Result<()> {
        let mut previous = 0;
        // SAFETY: self owns a send/dead-name reference; a nonzero notify comes from a live receive right.
        // The kernel acquires the MAKE_SEND_ONCE right; previous is the returned, exclusively owned send-once right.
        let code = unsafe {
            mach_port_request_notification(
                task_self(),
                self.name,
                NOTIFY_DEAD_NAME,
                0,
                notify,
                disposition,
                &mut previous,
            )
        };
        release_send(previous);
        check(code, "mach_port_request_notification")
    }

    pub fn send(
        &self,
        op: Operation,
        request: u64,
        body: &Value,
        reply: ReplyPort<'_>,
        timeout_ms: u32,
    ) -> Result<()> {
        // Serialize first so an invalid body cannot cause any additional rights to be acquired.
        let mut frame = Frame::json(op, request, body)?;
        let reply = match reply {
            ReplyPort::None => None,
            ReplyPort::Send(receive) => Some(ReplyRight::Send(receive.make_send()?)),
            ReplyPort::Once(receive) => Some(ReplyRight::Once(receive.make_send_once()?)),
        };
        let destination = self.try_clone()?;
        // Move the exclusively owned references into the header. The kernel consumes them on success;
        // post destroys the returned header on failure. Explicit MOVE dispositions avoid confusing
        // borrowed rights with rights returned by pseudo-copyout on failure.
        frame.header.remote_port = destination.into_raw();
        frame.header.bits = TYPE_MOVE_SEND;
        if let Some(reply) = reply {
            let (name, disposition) = reply.into_raw();
            frame.header.local_port = name;
            frame.header.bits |= disposition << 8;
        }
        post(frame, timeout_ms)
    }
}

impl SendOnceRight {
    pub fn name(&self) -> Port {
        self.name
    }

    fn into_raw(mut self) -> Port {
        let name = self.name;
        self.name = 0;
        name
    }

    pub fn send(self, op: Operation, request: u64, body: &Value, timeout_ms: u32) -> Result<()> {
        let mut frame = Frame::json(op, request, body)?;
        frame.header.bits = TYPE_MOVE_SEND_ONCE;
        frame.header.remote_port = self.into_raw();
        post(frame, timeout_ms)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ReplyPort<'a> {
    None,
    Send(&'a ReceiveRight),
    Once(&'a ReceiveRight),
}
#[derive(Debug)]
pub enum ReplyRight {
    Send(SendRight),
    Once(SendOnceRight),
}

impl ReplyRight {
    fn into_raw(self) -> (Port, u32) {
        match self {
            Self::Send(right) => (right.into_raw(), TYPE_MOVE_SEND),
            Self::Once(right) => (right.into_raw(), TYPE_MOVE_SEND_ONCE),
        }
    }
}

fn post(mut frame: Frame, timeout_ms: u32) -> Result<()> {
    let size = frame.header.size;
    // SAFETY: The pack(4) header is correctly aligned; the fully initialized buffer is at least size bytes long.
    // The header contains only moved, exclusively owned send/send-once rights, not borrowed rights or descriptors.
    let code = unsafe {
        mach_msg(
            ptr::addr_of_mut!(frame.header),
            SEND_MSG | SEND_TIMEOUT,
            size,
            0,
            0,
            timeout_ms,
            0,
        )
    };
    if code != 0 {
        // mach_msg_destroy cleans up using the receive layout and does not handle local_port.
        // After a failed send, local_port is a reply right owned by this module and must be released separately.
        release_send(frame.header.local_port);
        frame.header.local_port = 0;
        frame.header.bits &= !LOCAL_MASK;
        // SAFETY: The input is a simple message constructed by this module. On failure, Mach preserves
        // the original header or returns unconsumed rights through pseudo-copyout. In either case,
        // this header exclusively owns them. Use the returned header rather than cached old port names
        // to account for renaming and avoid double releases.
        unsafe {
            mach_msg_destroy(ptr::addr_of_mut!(frame.header));
        }
    }
    check(code, "mach_msg send")
}

#[derive(Debug)]
pub struct Message {
    pub local_port: Port,
    pub operation: Operation,
    pub request: u64,
    pub body: Value,
    pub audit: AuditToken,
    pub reply: Option<ReplyRight>,
}

#[derive(Debug)]
pub enum Event {
    Message(Message),
    DeadName(Port),
    Timeout,
    Invalid,
}

/// Like the upstream TCMessage, this is 16500 bytes and aligned to at least four bytes.
struct ReceiveBuffer {
    words: [u32; RECEIVE_CAPACITY / 4],
    destroy: bool,
}

impl ReceiveBuffer {
    fn new() -> Self {
        Self {
            words: [0; RECEIVE_CAPACITY / 4],
            destroy: false,
        }
    }

    fn header_ptr(&mut self) -> *mut Header {
        self.words.as_mut_ptr().cast()
    }

    fn header(&self) -> Header {
        // SAFETY: The u32 array is four-byte aligned and larger than Header; all bytes are initialized.
        unsafe { ptr::read(self.words.as_ptr().cast::<Header>()) }
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: All bytes in the u32 array are initialized; the slice covers only the array's actual capacity.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), RECEIVE_CAPACITY) }
    }

    fn take_reply(&mut self, kind: u32) -> Option<ReplyRight> {
        let name = self.words[2];
        // After reception, remote_port is the reply right carried by the message, not the destination receive right.
        self.words[2] = 0;
        self.words[0] &= !REMOTE_MASK;
        match kind {
            TYPE_MOVE_SEND => Some(ReplyRight::Send(SendRight { name })),
            TYPE_MOVE_SEND_ONCE => Some(ReplyRight::Once(SendOnceRight { name })),
            _ => None,
        }
    }
}

impl Drop for ReceiveBuffer {
    fn drop(&mut self) {
        if self.destroy {
            // After reception, local_port is the borrowed destination receive right; the message must never release it.
            self.words[3] = 0;
            self.words[0] &= !LOCAL_MASK;
            // SAFETY: destroy=true is set only for messages received successfully by the kernel or with BODY_ERROR.
            // The kernel validates descriptor layouts; take_reply has cleared rights transferred to RAII objects.
            // The header, descriptors, and OOL memory are still destroyed even if the protocol or JSON is invalid.
            unsafe {
                mach_msg_destroy(self.header_ptr());
            }
        }
    }
}

fn audit_trailer(bytes: &[u8], size: usize) -> Result<AuditToken> {
    ensure!(size % 4 == 0, "unaligned trailer offset");
    let trailer = bytes.get(size..).context("trailer offset exceeds buffer")?;
    ensure!(protocol::word(trailer, 0)? == 0, "invalid trailer format");
    let length = protocol::word(trailer, 4)? as usize;
    ensure!(
        (AUDIT_TRAILER_SIZE..=MAX_TRAILER_SIZE).contains(&length),
        "invalid audit trailer length"
    );
    ensure!(length <= trailer.len(), "truncated audit trailer");
    let mut val = [0u32; 8];
    for (index, value) in val.iter_mut().enumerate() {
        *value = protocol::word(trailer, 20 + index * 4)?;
    }
    Ok(AuditToken { val })
}

fn decode_received(buffer: &mut ReceiveBuffer) -> Result<Event> {
    let header = buffer.header();
    let size = header.size as usize;
    ensure!(
        size >= mem::size_of::<Header>() && size <= protocol::FRAME_CAPACITY,
        "invalid received size"
    );
    let audit = audit_trailer(buffer.bytes(), size)?;
    if header.id == NOTIFY_DEAD_NAME {
        ensure!(
            size == DEAD_NAME_MESSAGE_SIZE && header.bits & protocol::COMPLEX == 0,
            "invalid dead-name notification"
        );
        // The message ID alone can be forged. Validate the kernel audit token; never decrement references using a user-supplied name.
        ensure!(
            audit.val == [0; 8],
            "dead-name notification is not from kernel"
        );
        ensure!(
            header.remote_port == 0
                && header.voucher_port == 0
                && header.bits & (REMOTE_MASK | VOUCHER_MASK) == 0,
            "notification carries unexpected rights"
        );
        let dead = protocol::word(buffer.bytes(), 32)?;
        ensure!(valid(dead), "invalid dead-name notification name");
        // SAFETY: The kernel's DEAD_NAME notification adds an extra dead-name user reference to not_port.
        // The field is a PORT_NAME, not a descriptor; mach_msg_destroy does not release this extra reference.
        check(
            unsafe { mach_port_deallocate(task_self(), dead) },
            "release dead-name notification reference",
        )?;
        return Ok(Event::DeadName(dead));
    }
    let decoded = protocol::decode(buffer.bytes())?;
    ensure!(
        header.voucher_port == 0 && header.bits & VOUCHER_MASK == 0,
        "protocol frame carries voucher"
    );
    let kind = header.bits & REMOTE_MASK;
    let reply = if header.remote_port == 0 {
        ensure!(kind == 0, "reply disposition without right");
        None
    } else {
        ensure!(
            valid(header.remote_port) && matches!(kind, TYPE_MOVE_SEND | TYPE_MOVE_SEND_ONCE),
            "invalid reply right"
        );
        buffer.take_reply(kind)
    };
    Ok(Event::Message(Message {
        local_port: header.local_port,
        operation: decoded.operation,
        request: decoded.request,
        body: decoded.body,
        audit,
        reply,
    }))
}

pub fn receive(port: Port, timeout_ms: u32) -> Result<Event> {
    ensure!(valid(port), "invalid receive port name");
    let mut buffer = ReceiveBuffer::new();
    // SAFETY: The buffer is correctly aligned and writable, and rcv_size equals its capacity. The kernel validates the port name.
    // RCV_LARGE is not used: the kernel dequeues and destroys oversized messages instead of letting them block the next message.
    let code = unsafe {
        mach_msg(
            buffer.header_ptr(),
            RCV_MSG | RCV_TIMEOUT | RCV_TRAILER_AUDIT,
            0,
            RECEIVE_CAPACITY as u32,
            port,
            timeout_ms,
            0,
        )
    };
    match code & !MSG_ERROR_MASK {
        0 => {
            buffer.destroy = true;
            // Protocol errors are not syscall errors; Drop always cleans up message rights delivered by the kernel.
            Ok(decode_received(&mut buffer).unwrap_or(Event::Invalid))
        }
        RCV_TIMED_OUT => Ok(Event::Timeout),
        RCV_TOO_LARGE | RCV_HEADER_ERROR => Ok(Event::Invalid),
        RCV_BODY_ERROR => {
            // BODY_ERROR returns a received partial message that can be destroyed; the kernel has nulled missing rights.
            buffer.destroy = true;
            Ok(Event::Invalid)
        }
        _ => {
            check(code, "mach_msg receive")?;
            unreachable!()
        }
    }
}

pub fn lookup_service() -> Result<SendRight> {
    let service = std::ffi::CString::new(protocol::SERVICE).expect("constant has no NUL");
    let mut name = 0;
    // SAFETY: libSystem initializes bootstrap_port; service is a fixed, NUL-terminated name.
    // name is a valid output pointer; success returns a send right in this task.
    let code = unsafe { bootstrap_look_up(bootstrap_port, service.as_ptr(), &mut name) };
    let right = SendRight { name };
    check(code, "bootstrap_look_up")?;
    ensure!(valid(name), "bootstrap lookup returned invalid send right");
    Ok(right)
}

/// Provided as an implementation only; offline tests never call this function.
pub fn register_service(receive: &ReceiveRight) -> Result<()> {
    let service = std::ffi::CString::new(protocol::SERVICE).expect("constant has no NUL");
    let send = receive.make_send()?;
    // SAFETY: bootstrap_register's sp uses COPY_SEND. send stays alive throughout the call and is then released automatically.
    // The fixed name is NUL-terminated; no receive right is transferred to bootstrap.
    check(
        unsafe { bootstrap_register(bootstrap_port, service.as_ptr(), send.name) },
        "bootstrap_register",
    )
}

/// Acquires a send reference to the target receive right; releases the task port before returning.
/// This entry point accepts only a path-verified tccd; tests never call task_for_pid.
pub fn extract_send(pid: i32, target_name: Port) -> Result<SendRight> {
    ensure!(valid(target_name), "invalid target receive name");
    crate::process::verify_tccd(pid)?;
    let mut task = 0;
    // SAFETY: pid is verified and the output pointer is valid; the system still checks authorization and task acquisition.
    let code = unsafe { task_for_pid(task_self(), pid, &mut task) };
    let task = SendRight { name: task };
    check(code, "task_for_pid")?;
    ensure!(valid(task.name), "task_for_pid returned invalid task right");
    let mut name = 0;
    let mut kind = 0;
    // SAFETY: task is a task control send right owned by this scope. The output pointers are valid.
    // MAKE_SEND does not move the target receive right; task's Drop releases the task on every return path.
    let code = unsafe {
        mach_port_extract_right(task.name, target_name, TYPE_MAKE_SEND, &mut name, &mut kind)
    };
    let right = SendRight { name };
    check(code, "mach_port_extract_right")?;
    ensure!(
        valid(name) && kind == TYPE_MOVE_SEND,
        "port handoff returned unexpected right type: {kind}"
    );
    Ok(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::mem::{align_of, offset_of, size_of};

    extern "C" {
        fn mach_port_get_refs(task: Port, name: Port, right: u32, refs: *mut u32) -> i32;
    }

    fn refs(name: Port, right: u32) -> u32 {
        let mut count = 0;
        // SAFETY: Tests query only ports in this process; count is a valid output pointer.
        check(
            unsafe { mach_port_get_refs(task_self(), name, right, &mut count) },
            "test get refs",
        )
        .unwrap();
        count
    }

    fn name_exists(name: Port) -> bool {
        let mut kind = 0;
        // SAFETY: This only queries the name's type; the kernel returns an error for destroyed names.
        unsafe { mach_port_type(task_self(), name, &mut kind) == 0 }
    }

    fn message(event: Event) -> Message {
        match event {
            Event::Message(message) => message,
            event => panic!("expected Message, got {event:?}"),
        }
    }

    fn raw_frame(destination: &SendRight, mut frame: Frame, reply: Option<ReplyRight>) -> Frame {
        frame.header.remote_port = destination.try_clone().unwrap().into_raw();
        frame.header.bits = TYPE_MOVE_SEND;
        if let Some(reply) = reply {
            let (name, kind) = reply.into_raw();
            frame.header.local_port = name;
            frame.header.bits |= kind << 8;
        }
        frame
    }

    #[test]
    fn receive_buffer_and_audit_trailer_layout() {
        assert_eq!(RECEIVE_CAPACITY, 16_500);
        assert!(align_of::<ReceiveBuffer>() >= 4);
        #[repr(C)]
        struct Trailer {
            kind: u32,
            size: u32,
            seqno: u32,
            sender: [u32; 2],
            audit: AuditToken,
        }
        assert_eq!(size_of::<Trailer>(), AUDIT_TRAILER_SIZE);
        assert_eq!(offset_of!(Trailer, audit), 20);
        let token = AuditToken {
            val: [1, 2, 3, 4, 5, 6, 7, 8],
        };
        let mut bytes = vec![0u8; 4 + AUDIT_TRAILER_SIZE];
        bytes[8..12].copy_from_slice(&(AUDIT_TRAILER_SIZE as u32).to_ne_bytes());
        for (index, val) in token.val.iter().enumerate() {
            let offset = 4 + 20 + index * 4;
            bytes[offset..offset + 4].copy_from_slice(&val.to_ne_bytes());
        }
        assert_eq!(audit_trailer(&bytes, 4).unwrap(), token);
        assert!(audit_trailer(&bytes, 3).is_err());
        assert!(audit_trailer(&bytes, bytes.len() + 4).is_err());
        assert!(audit_trailer(&bytes[..bytes.len() - 1], 4).is_err());
        for size in [0u32, 8, 51, 69, u32::MAX] {
            let mut invalid = bytes.clone();
            invalid[8..12].copy_from_slice(&size.to_ne_bytes());
            assert!(audit_trailer(&invalid, 4).is_err());
        }
        bytes[4..8].copy_from_slice(&1u32.to_ne_bytes());
        assert!(audit_trailer(&bytes, 4).is_err());
    }

    #[test]
    fn clone_and_drop_preserve_receive_and_release_send_refs() {
        let receive = ReceiveRight::new().unwrap();
        let name = receive.name();
        let send = receive.make_send().unwrap();
        assert_eq!(refs(name, RIGHT_SEND), 1);
        let clone = send.try_clone().unwrap();
        assert_eq!(refs(name, RIGHT_SEND), 2);
        drop(clone);
        assert_eq!(refs(name, RIGHT_SEND), 1);
        drop(send);
        assert_eq!(refs(name, RIGHT_SEND), 0);
        assert_eq!(refs(name, RIGHT_RECEIVE), 1);
        drop(receive);
        assert!(!name_exists(name));
        let set = PortSet::new().unwrap();
        let set_name = set.name();
        drop(set);
        assert!(!name_exists(set_name));
    }

    #[test]
    fn loopback_all_operations_audit_and_timeout() {
        let receive = ReceiveRight::new().unwrap();
        let send = receive.make_send().unwrap();
        let body = json!({"ok": true, "unicode": "caf\u{e9}", "count": 123});
        for number in (1..=11).chain(std::iter::once(255)) {
            let op = Operation::try_from(number).unwrap();
            send.send(op, u64::MAX - number as u64, &body, ReplyPort::None, 100)
                .unwrap();
            let message = message(super::receive(receive.name(), 1000).unwrap());
            assert_eq!(message.local_port, receive.name());
            assert_eq!(message.operation, op);
            assert_eq!(message.request, u64::MAX - number as u64);
            assert_eq!(message.body, body);
            assert_eq!(message.audit.pid(), std::process::id() as i32);
            // SAFETY: geteuid only reads this process's identity.
            assert_eq!(message.audit.euid(), unsafe { libc::geteuid() });
            assert!(message.audit.pid_version() > 0);
            assert!(message.reply.is_none());
        }
        assert!(matches!(
            super::receive(receive.name(), 0).unwrap(),
            Event::Timeout
        ));
        assert_eq!(refs(send.name(), RIGHT_SEND), 1);
        assert_eq!(refs(receive.name(), RIGHT_RECEIVE), 1);
    }

    #[test]
    fn loopback_maximum_json_body_has_kernel_trailer() {
        let receive = ReceiveRight::new().unwrap();
        let send = receive.make_send().unwrap();
        let body = Value::String("x".repeat(protocol::MAX_BODY - 2));
        send.send(Operation::Status, u64::MAX, &body, ReplyPort::None, 100)
            .unwrap();
        assert_eq!(
            message(super::receive(receive.name(), 1000).unwrap()).body,
            body
        );
        assert_eq!(refs(send.name(), RIGHT_SEND), 1);
    }

    #[test]
    fn port_set_reports_actual_local_port_and_drop_keeps_members() {
        let receive = ReceiveRight::new().unwrap();
        let send = receive.make_send().unwrap();
        let set = PortSet::new().unwrap();
        set.add(&receive).unwrap();
        send.send(Operation::Status, 10, &json!({}), ReplyPort::None, 100)
            .unwrap();
        assert_eq!(
            message(super::receive(set.name(), 1000).unwrap()).local_port,
            receive.name()
        );
        drop(set);
        send.send(Operation::Status, 11, &json!({}), ReplyPort::None, 100)
            .unwrap();
        assert_eq!(
            message(super::receive(receive.name(), 1000).unwrap()).request,
            11
        );
    }

    #[test]
    fn reply_send_and_send_once_are_owned_and_consumed() {
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        destination
            .send(
                Operation::Status,
                7,
                &json!({}),
                ReplyPort::Once(&client),
                100,
            )
            .unwrap();
        let incoming = message(super::receive(server.name(), 1000).unwrap());
        let once = match incoming.reply.unwrap() {
            ReplyRight::Once(right) => right,
            _ => panic!("wrong reply kind"),
        };
        let once_name = once.name();
        assert_eq!(refs(once_name, 2), 1);
        once.send(Operation::Status, 7, &json!({"ok":true}), 100)
            .unwrap();
        assert!(!name_exists(once_name));
        assert_eq!(
            message(super::receive(client.name(), 1000).unwrap()).body,
            json!({"ok":true})
        );
        destination
            .send(
                Operation::Subscribe,
                8,
                &json!({}),
                ReplyPort::Send(&client),
                100,
            )
            .unwrap();
        let incoming = message(super::receive(server.name(), 1000).unwrap());
        let reply = match incoming.reply.unwrap() {
            ReplyRight::Send(right) => right,
            _ => panic!("wrong reply kind"),
        };
        assert_eq!(refs(client.name(), RIGHT_SEND), 1);
        for request in 1..=2 {
            reply
                .send(
                    Operation::BypassLog,
                    request,
                    &json!({"type":"record"}),
                    ReplyPort::None,
                    100,
                )
                .unwrap();
            assert_eq!(
                message(super::receive(client.name(), 1000).unwrap()).request,
                request
            );
        }
        drop(reply);
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert_eq!(refs(client.name(), RIGHT_RECEIVE), 1);
    }

    #[test]
    fn malformed_json_destroys_transferred_reply_right() {
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        let reply = client.make_send().unwrap();
        let frame = raw_frame(
            &destination,
            Frame::bytes(Operation::Status, 0, b"not JSON").unwrap(),
            Some(ReplyRight::Send(reply)),
        );
        post(frame, 100).unwrap();
        assert!(matches!(
            super::receive(server.name(), 1000).unwrap(),
            Event::Invalid
        ));
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert_eq!(refs(server.name(), RIGHT_SEND), 1);
        assert_eq!(refs(server.name(), RIGHT_RECEIVE), 1);
    }

    #[test]
    fn invalid_protocol_and_unknown_operation_are_destroyed() {
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        for bad in 0..4 {
            let mut frame = Frame::json(Operation::Status, 0, &json!({})).unwrap();
            match bad {
                0 => frame.magic = 0,
                1 => frame.version = 2,
                2 => frame.operation = 99,
                _ => frame.length = u32::MAX,
            }
            let frame = raw_frame(
                &destination,
                frame,
                Some(ReplyRight::Send(client.make_send().unwrap())),
            );
            post(frame, 100).unwrap();
            assert!(matches!(
                super::receive(server.name(), 1000).unwrap(),
                Event::Invalid
            ));
            assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        }
    }

    #[test]
    fn timeout_destroys_pseudo_copyout_reply_rights() {
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        for request in 0..128 {
            destination
                .send(Operation::Status, request, &json!({}), ReplyPort::None, 0)
                .unwrap();
        }
        let error = destination
            .send(
                Operation::Status,
                999,
                &json!({}),
                ReplyPort::Send(&client),
                0,
            )
            .unwrap_err();
        assert!(is_timeout(&error));
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert_eq!(refs(destination.name(), RIGHT_SEND), 1);
        assert_eq!(refs(client.name(), RIGHT_RECEIVE), 1);
        // Also verify failure cleanup for explicit rights created with MAKE_SEND_ONCE.
        let error = destination
            .send(
                Operation::Status,
                1000,
                &json!({}),
                ReplyPort::Once(&client),
                0,
            )
            .unwrap_err();
        assert!(is_timeout(&error));
        assert_eq!(refs(destination.name(), RIGHT_SEND), 1);
        // Abandoning a send-once right generates a kernel SEND_ONCE notification, not a control protocol frame.
        assert!(matches!(
            super::receive(client.name(), 1000).unwrap(),
            Event::Invalid
        ));
        assert_eq!(refs(client.name(), RIGHT_RECEIVE), 1);
    }

    #[test]
    fn destination_death_and_notification_release_extra_reference() {
        let server = ReceiveRight::new().unwrap();
        let notifications = ReceiveRight::new().unwrap();
        let send = server.make_send().unwrap();
        let name = send.name();
        send.watch_dead(&notifications).unwrap();
        drop(server);
        match super::receive(notifications.name(), 1000).unwrap() {
            Event::DeadName(dead) => assert_eq!(dead, name),
            event => panic!("expected kernel dead-name notification, got {event:?}"),
        }
        assert_eq!(refs(name, 4), 1);
        let error = send
            .send(Operation::Status, 0, &json!({}), ReplyPort::None, 0)
            .unwrap_err();
        assert!(is_invalid_destination(&error));
        assert!(!is_timeout(&error));
        drop(send);
        assert!(!name_exists(name));
    }

    #[test]
    fn unwatch_releases_previous_send_once_and_no_dead_notification() {
        let server = ReceiveRight::new().unwrap();
        let notifications = ReceiveRight::new().unwrap();
        let send = server.make_send().unwrap();
        send.watch_dead(&notifications).unwrap();
        send.unwatch_dead().unwrap();
        // Releasing the canceled notification's send-once right sends a SEND_ONCE notification.
        assert!(matches!(
            super::receive(notifications.name(), 1000).unwrap(),
            Event::Invalid
        ));
        drop(server);
        assert_eq!(refs(send.name(), 4), 1);
        assert!(matches!(
            super::receive(notifications.name(), 0).unwrap(),
            Event::Timeout
        ));
    }

    #[test]
    fn forged_dead_name_cannot_release_any_reference() {
        let server = ReceiveRight::new().unwrap();
        let victim = ReceiveRight::new().unwrap();
        let send = server.make_send().unwrap();
        let victim_send = victim.make_send().unwrap();
        #[repr(C)]
        struct Notification {
            header: Header,
            ndr: [u8; 8],
            name: Port,
        }
        assert_eq!(size_of::<Notification>(), DEAD_NAME_MESSAGE_SIZE);
        let mut fake = Notification {
            header: Header {
                bits: TYPE_MOVE_SEND,
                size: DEAD_NAME_MESSAGE_SIZE as u32,
                remote_port: send.try_clone().unwrap().into_raw(),
                id: NOTIFY_DEAD_NAME,
                ..Header::default()
            },
            ndr: [0; 8],
            name: victim_send.name(),
        };
        // SAFETY: This test constructs a valid simple Mach message, exclusively owns the destination send reference, and uses an exactly sized buffer.
        let code = unsafe {
            mach_msg(
                &mut fake.header,
                SEND_MSG | SEND_TIMEOUT,
                size_of::<Notification>() as u32,
                0,
                0,
                100,
                0,
            )
        };
        if code != 0 {
            // SAFETY: The unsent, valid simple header still owns the destination send reference.
            unsafe {
                mach_msg_destroy(&mut fake.header);
            }
        }
        check(code, "test forged notification send").unwrap();
        assert!(matches!(
            super::receive(server.name(), 1000).unwrap(),
            Event::Invalid
        ));
        assert_eq!(refs(victim_send.name(), RIGHT_SEND), 1);
    }

    #[test]
    fn complex_invalid_message_destroys_port_descriptor() {
        #[repr(C)]
        struct Descriptor {
            name: Port,
            pad1: u32,
            pad2: u16,
            disposition: u8,
            kind: u8,
        }
        #[repr(C)]
        struct ComplexMessage {
            header: Header,
            count: u32,
            descriptor: Descriptor,
        }
        assert_eq!(size_of::<Descriptor>(), 12);
        assert_eq!(size_of::<ComplexMessage>(), 40);
        let server = ReceiveRight::new().unwrap();
        let victim = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        let victim_send = victim.make_send().unwrap();
        let mut complex = ComplexMessage {
            header: Header {
                bits: TYPE_MOVE_SEND | protocol::COMPLEX,
                size: size_of::<ComplexMessage>() as u32,
                remote_port: destination.try_clone().unwrap().into_raw(),
                id: protocol::MAGIC as i32,
                ..Header::default()
            },
            count: 1,
            descriptor: Descriptor {
                name: victim_send.try_clone().unwrap().into_raw(),
                pad1: 0,
                pad2: 0,
                disposition: TYPE_MOVE_SEND as u8,
                kind: 0,
            },
        };
        // SAFETY: The SDK descriptor layout and total size are verified; the header and descriptor each exclusively own a send reference.
        let code = unsafe {
            mach_msg(
                &mut complex.header,
                SEND_MSG | SEND_TIMEOUT,
                size_of::<ComplexMessage>() as u32,
                0,
                0,
                100,
                0,
            )
        };
        if code != 0 {
            // SAFETY: This test constructs the entire complex message; the returned header/descriptors still own unsent rights.
            unsafe {
                mach_msg_destroy(&mut complex.header);
            }
        }
        check(code, "test complex send").unwrap();
        assert!(matches!(
            super::receive(server.name(), 1000).unwrap(),
            Event::Invalid
        ));
        assert_eq!(refs(victim_send.name(), RIGHT_SEND), 1);
        assert_eq!(refs(destination.name(), RIGHT_SEND), 1);
    }

    #[test]
    fn send_once_drop_dead_destination_and_encode_failure_release_right() {
        let target = ReceiveRight::new().unwrap();
        let once = target.make_send_once().unwrap();
        let name = once.name();
        drop(once);
        assert!(!name_exists(name));
        assert!(matches!(
            super::receive(target.name(), 1000).unwrap(),
            Event::Invalid
        ));
        let once = target.make_send_once().unwrap();
        let name = once.name();
        let body = Value::String("x".repeat(protocol::MAX_BODY));
        assert!(once.send(Operation::Status, 0, &body, 0).is_err());
        assert!(!name_exists(name));
        assert!(matches!(
            super::receive(target.name(), 1000).unwrap(),
            Event::Invalid
        ));
        let once = target.make_send_once().unwrap();
        let name = once.name();
        drop(target);
        // The local kernel treats a send to a dead send-once destination as successful and consumes the right.
        once.send(Operation::Status, 0, &json!({}), 0).unwrap();
        assert!(!name_exists(name));
    }

    #[test]
    fn invalid_names_and_oversized_body_are_rejected_without_effects() {
        assert!(super::receive(0, 0).is_err());
        assert!(super::receive(PORT_DEAD, 0).is_err());
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let send = server.make_send().unwrap();
        let body = Value::String("x".repeat(protocol::MAX_BODY));
        assert!(send
            .send(Operation::Status, 0, &body, ReplyPort::Send(&client), 0)
            .is_err());
        assert_eq!(refs(send.name(), RIGHT_SEND), 1);
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert!(matches!(
            super::receive(server.name(), 0).unwrap(),
            Event::Timeout
        ));
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use serde_json::json;
    use std::mem::size_of;

    extern "C" {
        fn mach_port_get_refs(task: Port, name: Port, right: u32, refs: *mut u32) -> i32;
    }

    fn refs(name: Port, right: u32) -> u32 {
        let mut count = 0;
        // SAFETY: This queries names owned by this process; count is a valid output pointer.
        check(
            unsafe { mach_port_get_refs(task_self(), name, right, &mut count) },
            "test get refs",
        )
        .unwrap();
        count
    }

    #[test]
    fn oversized_received_message_is_destroyed_and_does_not_poison_queue() {
        #[repr(C)]
        struct Oversized {
            header: Header,
            padding: [u32; RECEIVE_CAPACITY / 4],
        }
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        let mut oversized = Oversized {
            header: Header {
                bits: TYPE_MOVE_SEND | (TYPE_MOVE_SEND << 8),
                size: size_of::<Oversized>() as u32,
                remote_port: destination.try_clone().unwrap().into_raw(),
                local_port: client.make_send().unwrap().into_raw(),
                id: protocol::MAGIC as i32,
                ..Header::default()
            },
            padding: [0; RECEIVE_CAPACITY / 4],
        };
        // SAFETY: The entire buffer is four-byte aligned; the simple header exclusively owns the destination and reply send references.
        let code = unsafe {
            mach_msg(
                &mut oversized.header,
                SEND_MSG | SEND_TIMEOUT,
                size_of::<Oversized>() as u32,
                0,
                0,
                100,
                0,
            )
        };
        if code != 0 {
            release_send(oversized.header.local_port);
            oversized.header.local_port = 0;
            oversized.header.bits &= !LOCAL_MASK;
            // SAFETY: The unsent, valid simple header still owns the destination send reference.
            unsafe {
                mach_msg_destroy(&mut oversized.header);
            }
        }
        check(code, "test oversized send").unwrap();
        assert!(matches!(
            receive(server.name(), 1000).unwrap(),
            Event::Invalid
        ));
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert_eq!(refs(destination.name(), RIGHT_SEND), 1);
        destination
            .send(
                Operation::Status,
                42,
                &json!({"ok":true}),
                ReplyPort::None,
                100,
            )
            .unwrap();
        match receive(server.name(), 1000).unwrap() {
            Event::Message(message) => assert_eq!(message.request, 42),
            event => panic!("queue remained poisoned: {event:?}"),
        }
    }

    #[test]
    fn message_drop_releases_reply_without_releasing_receive() {
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        destination
            .send(
                Operation::Status,
                0,
                &json!({}),
                ReplyPort::Send(&client),
                100,
            )
            .unwrap();
        let event = receive(server.name(), 1000).unwrap();
        assert!(matches!(
            &event,
            Event::Message(Message {
                reply: Some(ReplyRight::Send(_)),
                ..
            })
        ));
        assert_eq!(refs(client.name(), RIGHT_SEND), 1);
        drop(event);
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert_eq!(refs(client.name(), RIGHT_RECEIVE), 1);
    }

    #[test]
    fn dead_destination_error_drops_temporary_reply_right() {
        let server = ReceiveRight::new().unwrap();
        let client = ReceiveRight::new().unwrap();
        let destination = server.make_send().unwrap();
        drop(server);
        let error = destination
            .send(
                Operation::Status,
                0,
                &json!({}),
                ReplyPort::Send(&client),
                0,
            )
            .unwrap_err();
        assert!(is_invalid_destination(&error));
        assert_eq!(refs(client.name(), RIGHT_SEND), 0);
        assert_eq!(refs(client.name(), RIGHT_RECEIVE), 1);
    }
}
