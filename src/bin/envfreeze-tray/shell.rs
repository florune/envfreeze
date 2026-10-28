//! 与桌面交互的小工具：打开文件、激活窗口、消息框、开机自启、本地时间。

use std::path::Path;

use anyhow::{Result, bail};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, IsIconic, MB_ICONERROR, MB_OK, MessageBoxW, SW_RESTORE, SW_SHOWNORMAL,
    SetForegroundWindow, ShowWindow,
};
use windows::core::{HSTRING, PCWSTR, w};

/// 用默认程序打开文件或目录；没有关联程序的文件（如 .toml）退回到记事本。
pub fn open(path: &Path) {
    let file = HSTRING::from(path.as_os_str());
    let result = unsafe { ShellExecuteW(None, w!("open"), &file, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute 返回值 <= 32 表示失败。
    if result.0 as isize <= 32 && path.is_file() {
        let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
    }
}

/// 找到标题为 `title` 的窗口并激活，找不到返回 false。
pub fn focus_window(title: &str) -> bool {
    let Ok(hwnd) = (unsafe { FindWindowW(PCWSTR::null(), &HSTRING::from(title)) }) else { return false };
    activate(hwnd);
    true
}

fn activate(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// 允许刚启动的进程把自己的窗口切到前台（托盘刚收到用户输入，有权转交这个权限）。
pub fn allow_foreground(pid: u32) {
    let _ = unsafe { AllowSetForegroundWindow(pid) };
}

pub fn error_box(text: &str) {
    unsafe { MessageBoxW(None, &HSTRING::from(text), w!("envfreeze"), MB_OK | MB_ICONERROR) };
}

/// 本地时间 `HH:MM:SS`。
pub fn local_time() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("envfreeze-tray");

/// 开机自启：写在当前用户的 Run 注册表项里，不需要管理员权限。
pub fn autostart_enabled() -> bool {
    let mut size = 0u32;
    let status =
        unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)) };
    status == ERROR_SUCCESS
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let status = if enabled {
        let exe = std::env::current_exe()?;
        let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) }
    };
    if status != ERROR_SUCCESS && !(status.0 == 2 && !enabled) {
        bail!("修改开机自启失败：{}", windows::core::Error::from(status.to_hresult()).message());
    }
    Ok(())
}
