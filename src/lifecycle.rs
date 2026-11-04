//! 后台 broker 不依赖已关闭的终端；只记录生命周期，不记录权限事件。
use std::ffi::CString;

fn message(text: &str) -> CString {
    CString::new(format!("xtcc: {}", text.replace('\0', "?"))).expect("已替换所有 NUL 字节")
}

pub(crate) fn log(text: &str) {
    let text = message(text);
    // SAFETY: format 固定且含一个 %s；参数是有效、存活的 NUL 结尾字符串。
    // syslog 只在本进程同步调用期间读取此指针。
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
