//! Win32 小工具：自动关闭的句柄、宽字符串、错误码判断。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, WIN32_ERROR};
use windows::core::HRESULT;

/// 离开作用域时自动 CloseHandle。
pub struct OwnedHandle(HANDLE);

impl OwnedHandle {
    pub fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // 关闭失败无法补救，忽略即可。
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// 以 NUL 结尾的 UTF-16 字符串。
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn is_win32(err: &windows::core::Error, code: WIN32_ERROR) -> bool {
    err.code() == HRESULT::from_win32(code.0)
}

pub fn is_access_denied(err: &windows::core::Error) -> bool {
    err.code() == E_ACCESSDENIED
}

/// 把 Win32 错误转成面向用户的说明；拒绝访问时附带管理员权限提示。
pub fn describe(err: &windows::core::Error) -> String {
    if is_access_denied(err) {
        format!("{} （拒绝访问：目标进程可能以管理员身份运行，请在管理员终端中执行）", err.message())
    } else {
        format!("{} (0x{:08X})", err.message(), err.code().0)
    }
}
