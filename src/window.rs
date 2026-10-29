//! 顶层窗口：冻结前隐藏，恢复后重新显示，效果类似切换虚拟桌面；关闭环境时关闭窗口。
//!
//! 隐藏和显示都用 ShowWindowAsync：它只往窗口所属线程投递一条消息并立即返回，
//! 不会因为目标进程卡住或已挂起而阻塞。代价是消息要等目标线程处理才生效，
//! 所以必须先隐藏、确认生效后再挂起；恢复时先恢复进程、再显示窗口。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use tracing::debug;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, SW_HIDE, SW_SHOWNA, SetForegroundWindow,
    ShowWindowAsync, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use crate::state::HiddenWindow;

const POLL: Duration = Duration::from_millis(15);

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut _)
}

fn owner_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // 跨进程时直接读取系统保存的标题，不发送 WM_GETTEXT，目标进程卡住也不会阻塞。
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..len])
}

/// 按任务栏的规则判断窗口会不会出现在任务栏和 Alt+Tab 里。
fn in_taskbar(hwnd: HWND) -> bool {
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & WS_EX_APPWINDOW.0 != 0 {
        return true;
    }
    ex & WS_EX_TOOLWINDOW.0 == 0 && unsafe { GetWindow(hwnd, GW_OWNER) }.is_err()
}

/// 窗口句柄仍然存在，并且还属于原来的进程（句柄可能被复用）。
fn still_ours(w: &HiddenWindow) -> bool {
    let h = hwnd(w.hwnd);
    unsafe { IsWindow(Some(h)) }.as_bool() && owner_pid(h) == w.pid
}

fn is_visible(raw: isize) -> bool {
    unsafe { IsWindowVisible(hwnd(raw)) }.as_bool()
}

/// 这些进程当前可见的顶层窗口，按 Z 序从上到下排列。
///
/// 对话框、工具窗口、输入法窗口等也都包括在内：只隐藏主窗口的话，它们冻结后会变成"未响应"。
pub fn visible(pids: &HashSet<u32>) -> Vec<HiddenWindow> {
    struct Ctx<'a> {
        pids: &'a HashSet<u32>,
        found: Vec<HiddenWindow>,
    }
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let pid = owner_pid(hwnd);
            if ctx.pids.contains(&pid) {
                let title = title(hwnd);
                let in_taskbar = in_taskbar(hwnd) && !title.is_empty();
                ctx.found.push(HiddenWindow { hwnd: hwnd.0 as isize, pid, in_taskbar, title });
            }
        }
        BOOL(1)
    }
    let mut ctx = Ctx { pids, found: Vec::new() };
    let _ = unsafe { EnumWindows(Some(callback), LPARAM(&raw mut ctx as isize)) };
    ctx.found
}

/// 前台窗口，属于这些进程时返回它的句柄。
pub fn foreground_of(pids: &HashSet<u32>) -> Option<isize> {
    let h = unsafe { GetForegroundWindow() };
    (!h.is_invalid() && pids.contains(&owner_pid(h))).then_some(h.0 as isize)
}

pub fn hide(windows: &[HiddenWindow]) {
    for w in windows {
        let _ = unsafe { ShowWindowAsync(hwnd(w.hwnd), SW_HIDE) };
    }
}

/// 等待窗口都隐藏，返回超时后仍然可见的窗口数（目标进程未响应时会这样）。
pub fn wait_hidden(windows: &[HiddenWindow], timeout: Duration) -> usize {
    let started = Instant::now();
    loop {
        let pending = windows.iter().filter(|w| is_visible(w.hwnd) && still_ours(w)).count();
        if pending == 0 || started.elapsed() >= timeout {
            debug!(pending, elapsed_ms = started.elapsed().as_millis(), "隐藏窗口");
            return pending;
        }
        std::thread::sleep(POLL);
    }
}

/// 重新显示窗口（保持原来的大小、位置、最大化/最小化状态，不抢焦点），
/// 然后把 `foreground` 切到前台。返回重新显示的任务栏窗口数。
pub fn show(windows: &[HiddenWindow], foreground: Option<isize>, timeout: Duration) -> usize {
    let alive: Vec<&HiddenWindow> = windows.iter().filter(|w| still_ours(w)).collect();
    // 从下往上显示，最后显示的在最上面。
    for w in alive.iter().rev() {
        let _ = unsafe { ShowWindowAsync(hwnd(w.hwnd), SW_SHOWNA) };
    }
    // 原前台窗口没了就退而求其次，选最上面一个任务栏窗口。
    let target = foreground
        .filter(|&f| alive.iter().any(|w| w.hwnd == f))
        .or_else(|| alive.iter().find(|w| w.in_taskbar).map(|w| w.hwnd));
    if let Some(target) = target {
        // 进程刚恢复，内存还在换入，可能要过一会儿才处理显示消息。
        let started = Instant::now();
        while !is_visible(target) && started.elapsed() < timeout {
            std::thread::sleep(POLL);
        }
        // 只有调用方有前台权限时才会成功（托盘刚收到快捷键、面板刚被点击）；
        // 否则系统只会让任务栏按钮闪烁，不影响窗口显示。
        let ok = unsafe { SetForegroundWindow(hwnd(target)) }.as_bool();
        debug!(ok, "激活原前台窗口");
    }
    alive.iter().filter(|w| w.in_taskbar).count()
}

/// 请求关闭窗口，等同于用户点了标题栏上的 X：程序可以保存、询问或拒绝。
pub fn close(w: &HiddenWindow) {
    if still_ours(w) {
        let _ = unsafe { PostMessageW(Some(hwnd(w.hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}
