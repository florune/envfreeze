//! 扫描可加入环境的程序：正在运行的窗口程序、开始菜单快捷方式。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::{CommandLineToArgvW, IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, HSTRING, Interface, PWSTR};

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub path: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// 补充说明：窗口标题或快捷方式所在位置。
    pub detail: String,
}

/// 当前有可见窗口的程序（按可执行文件去重）。
pub fn running_apps() -> Vec<AppEntry> {
    let mut windows: Vec<(u32, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_window), LPARAM(&raw mut windows as isize));
    }
    let own = std::env::current_exe().ok();
    let mut by_path: BTreeMap<String, AppEntry> = BTreeMap::new();
    for (pid, title) in windows {
        let Some(path) = image_path(pid) else { continue };
        if own.as_deref() == Some(path.as_path()) || is_system_path(&path) {
            continue;
        }
        let key = path.display().to_string().to_lowercase();
        let entry = by_path.entry(key).or_insert_with(|| AppEntry {
            name: file_stem(&path),
            path: path.clone(),
            args: Vec::new(),
            cwd: None,
            detail: String::new(),
        });
        if entry.detail.chars().count() < 80 {
            if !entry.detail.is_empty() {
                entry.detail.push_str(" | ");
            }
            entry.detail.push_str(&title);
        }
    }
    let mut apps: Vec<AppEntry> = by_path.into_values().collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = unsafe { &mut *(lparam.0 as *mut Vec<(u32, String)>) };
    unsafe {
        // 只要普通的顶层窗口：可见、无所有者、不是工具窗口、有标题。
        let is_tool = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
        if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) || is_tool {
            return BOOL(1);
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len <= 0 {
            return BOOL(1);
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        windows.push((pid, String::from_utf16_lossy(&buf[..len as usize])));
    }
    BOOL(1)
}

fn image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

/// 系统组件和打包应用（WindowsApps）不能由本工具启动并加入环境，不列出。
fn is_system_path(path: &Path) -> bool {
    let p = path.display().to_string().to_lowercase();
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_lowercase();
    p.starts_with(&windir) || p.contains(r"\windowsapps\")
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 开始菜单（当前用户 + 所有用户）里指向 .exe 的快捷方式。
pub fn start_menu_apps() -> Vec<AppEntry> {
    // 解析 .lnk 需要 COM；面板会在后台线程调用，这里为本线程初始化。
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let mut roots = Vec::new();
    if let Some(dir) = dirs::data_dir() {
        roots.push(dir.join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(dir) = std::env::var_os("ProgramData") {
        roots.push(PathBuf::from(dir).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    let mut links = Vec::new();
    for root in &roots {
        walk(root, &mut links);
    }

    let mut by_target: BTreeMap<String, AppEntry> = BTreeMap::new();
    for link in links {
        let Some(mut app) = resolve_link(&link) else { continue };
        let target = app.path.display().to_string().to_lowercase();
        let name = app.name.to_lowercase();
        if !target.ends_with(".exe")
            || !app.path.exists()
            || is_system_path(&app.path)
            || name.contains("uninstall")
            || name.contains("卸载")
        {
            continue;
        }
        app.detail =
            link.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        by_target.entry(target).or_insert(app);
    }
    let mut apps: Vec<AppEntry> = by_target.into_values().collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// 常用终端（在 PATH 中查找）。每个终端程序启动时都有自己的控制台窗口，
/// Claude Code、codex 在里面运行即可跟着进入环境。
pub fn terminal_apps() -> Vec<AppEntry> {
    let candidates =
        [("PowerShell 7", "pwsh.exe"), ("Windows PowerShell", "powershell.exe"), ("命令提示符", "cmd.exe")];
    candidates
        .iter()
        .filter_map(|(name, exe)| {
            let path = find_in_path(exe)?;
            Some(AppEntry {
                name: (*name).into(),
                path,
                args: Vec::new(),
                cwd: None,
                detail: "独立控制台窗口，记得设置工作目录".into(),
            })
        })
        .collect()
}

fn find_in_path(exe: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(exe)).find(|p| p.is_file())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
            out.push(path);
        }
    }
}

fn resolve_link(link: &Path) -> Option<AppEntry> {
    unsafe {
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = shell_link.cast().ok()?;
        file.Load(&HSTRING::from(link.as_os_str()), STGM_READ).ok()?;

        let mut buf = [0u16; 1024];
        shell_link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let path = PathBuf::from(from_wide(&buf));
        if path.as_os_str().is_empty() {
            return None;
        }
        let mut args = [0u16; 2048];
        let _ = shell_link.GetArguments(&mut args);
        let mut cwd = [0u16; 1024];
        let _ = shell_link.GetWorkingDirectory(&mut cwd);
        let cwd = from_wide(&cwd);
        Some(AppEntry {
            name: file_stem(link),
            path,
            args: split_args(&from_wide(&args)),
            cwd: (!cwd.is_empty()).then_some(cwd),
            detail: String::new(),
        })
    }
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// 按 Windows 规则把参数字符串拆成参数列表。
fn split_args(args: &str) -> Vec<String> {
    if args.trim().is_empty() {
        return Vec::new();
    }
    // 前面垫一个程序名：CommandLineToArgvW 对第一个参数有特殊处理。
    let line = HSTRING::from(format!("x {args}"));
    let mut count = 0;
    unsafe {
        let argv = CommandLineToArgvW(&line, &mut count);
        if argv.is_null() {
            return vec![args.to_owned()];
        }
        let parts = std::slice::from_raw_parts(argv, count as usize);
        let result = parts.iter().skip(1).map(|p| p.to_string().unwrap_or_default()).collect();
        let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(argv.cast())));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_args_like_windows() {
        assert_eq!(split_args(r#"--a "b c" d"#), ["--a", "b c", "d"]);
        assert!(split_args("  ").is_empty());
    }

    #[test]
    fn scans_without_panicking() {
        let running = running_apps();
        let start_menu = start_menu_apps();
        assert!(start_menu.iter().all(|a| a.path.extension().is_some()));
        eprintln!("运行中 {} 个：{:?}", running.len(), running.iter().map(|a| &a.name).collect::<Vec<_>>());
        eprintln!("开始菜单 {} 个：{:?}", start_menu.len(), start_menu.iter().map(|a| &a.name).collect::<Vec<_>>());
    }
}
