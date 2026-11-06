//! The background broker does not rely on a closed terminal; log lifecycle events, not permission events.
use std::ffi::CString;

fn message(text: &str) -> CString {
    CString::new(format!("xtcc: {}", text.replace('\0', "?")))
        .expect("all NUL bytes have been replaced")
}

pub(crate) fn log(text: &str) {
    let text = message(text);
    // SAFETY: The fixed format contains one %s; its argument is a valid, live NUL-terminated string.
    // syslog reads this pointer only during the synchronous call in this process.
    unsafe {
        libc::syslog(libc::LOG_NOTICE, c"%s".as_ptr(), text.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lifecycle_message_is_literal_and_nul_free() {
        let value = super::message("connection %s\0failed");
        assert_eq!(value.to_str().unwrap(), "xtcc: connection %s?failed");
    }
}
