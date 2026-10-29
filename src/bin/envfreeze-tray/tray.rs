//! 常驻托盘：图标、右键菜单、全局快捷键。
//!
//! 主线程只跑 Win32 消息循环（托盘图标和全局快捷键都依赖它），
//! 冻结/恢复/启动等耗时操作交给一个工作线程按顺序执行，完成后弹系统通知。
//! 配置文件每 2 秒检查一次修改时间，面板或手动改了配置会自动重新注册快捷键。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

use anyhow::{Context, Result};
use envfreeze::config::{self, Config};
use envfreeze::paths;
use envfreeze::status::{self, EnvState};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tracing::{debug, info, warn};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, SetTimer, TranslateMessage, WM_APP, WM_TIMER,
};
use windows::core::w;

use crate::ops::{self, EnvOp};
use crate::{icon, panel, shell};

/// 工作线程完成一次操作后通知主线程刷新状态。
const WM_REFRESH: u32 = WM_APP + 1;
const POLL_INTERVAL_MS: u32 = 2000;
/// 子菜单里最多列出这么多个隐藏窗口的标题。
const MAX_LISTED_WINDOWS: usize = 12;

/// 菜单需要的环境状态：名字、状态、冻结时隐藏的窗口标题。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvView {
    name: String,
    state: EnvState,
    hidden_windows: Vec<String>,
}

#[derive(Debug, Clone)]
enum Command {
    Env(EnvOp),
    OpenPanel,
    EditConfig,
    OpenLogs,
    ToggleAutostart,
    Quit,
}

pub fn run(config_path: PathBuf) -> Result<()> {
    // 单实例：托盘已在运行时，再次启动只是打开面板。
    let _mutex = unsafe { CreateMutexW(None, false, w!("Local\\envfreeze_tray")) }?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        info!("托盘已在运行，改为打开面板");
        return spawn_panel(&config_path).map(drop);
    }

    let main_thread = unsafe { GetCurrentThreadId() };
    let (tx, rx) = mpsc::channel();
    let worker_config = config_path.clone();
    std::thread::spawn(move || worker(&worker_config, rx, main_thread));

    let mut tray = Tray::new(config_path, tx)?;
    unsafe { SetTimer(None, 0, POLL_INTERVAL_MS, None) };
    info!("托盘已启动");

    let mut msg = MSG::default();
    loop {
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        match msg.message {
            WM_TIMER => tray.poll(),
            WM_REFRESH => {
                tray.busy = None;
                tray.poll();
            }
            _ => {}
        }
        if !tray.pump() {
            break;
        }
    }
    info!("托盘退出");
    Ok(())
}

/// 工作线程：顺序执行环境操作并弹出通知。
fn worker(config_path: &Path, rx: Receiver<EnvOp>, main_thread: u32) {
    for op in rx {
        info!(op = op.describe(), "开始执行");
        let result = ops::execute(config_path, &op);
        if result.ok {
            info!(op = op.describe(), "{}", result.body);
        } else {
            warn!(op = op.describe(), "{}", result.body);
        }
        let title = if result.ok { result.title } else { format!("{}（有错误）", result.title) };
        if let Err(e) = notify_rust::Notification::new().summary(&title).body(&result.body).show() {
            warn!("发送通知失败：{e}");
        }
        let _ = unsafe { PostThreadMessageW(main_thread, WM_REFRESH, WPARAM(0), LPARAM(0)) };
    }
}

fn spawn_panel(config_path: &Path) -> Result<Child> {
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .arg("--panel")
        .arg("--config")
        .arg(config_path)
        .spawn()
        .context("启动面板失败")?;
    shell::allow_foreground(child.id());
    Ok(child)
}

struct Tray {
    config_path: PathBuf,
    config: Option<Config>,
    config_error: Option<String>,
    config_mtime: Option<SystemTime>,
    states: Vec<EnvView>,
    icon: TrayIcon,
    hotkey_manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    hotkey_commands: HashMap<u32, Command>,
    menu_commands: HashMap<MenuId, Command>,
    autostart_item: Option<CheckMenuItem>,
    worker: Sender<EnvOp>,
    /// 正在执行的操作，显示在提示文字里。
    busy: Option<String>,
    panel: Option<Child>,
}

impl Tray {
    fn new(config_path: PathBuf, worker: Sender<EnvOp>) -> Result<Self> {
        let icon = TrayIconBuilder::new()
            .with_icon(icon::render(icon::color_for(&[])))
            .with_tooltip("envfreeze")
            .with_menu_on_left_click(false)
            .build()
            .context("创建托盘图标失败")?;
        let mut tray = Self {
            config_path,
            config: None,
            config_error: None,
            config_mtime: None,
            states: Vec::new(),
            icon,
            hotkey_manager: GlobalHotKeyManager::new().context("初始化全局快捷键失败")?,
            registered: Vec::new(),
            hotkey_commands: HashMap::new(),
            menu_commands: HashMap::new(),
            autostart_item: None,
            worker,
            busy: None,
            panel: None,
        };
        tray.reload_config();
        tray.refresh(true);
        Ok(tray)
    }

    /// 定时检查：配置变化则重载，环境状态变化则刷新图标和菜单。
    fn poll(&mut self) {
        let mtime = std::fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
        let config_changed = mtime != self.config_mtime;
        if config_changed {
            self.reload_config();
        }
        self.refresh(config_changed);
    }

    fn reload_config(&mut self) {
        self.config_mtime = std::fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
        match Config::load(&self.config_path) {
            Ok(config) => {
                info!("已加载配置 {}", self.config_path.display());
                self.config = Some(config);
                self.config_error = None;
            }
            Err(e) => {
                warn!("{e:#}");
                // 配置写坏时保留上一份可用配置，快捷键继续可用。
                self.config_error = Some(format!("{e:#}"));
            }
        }
        self.register_hotkeys();
    }

    fn register_hotkeys(&mut self) {
        let _ = self.hotkey_manager.unregister_all(&self.registered);
        self.registered.clear();
        self.hotkey_commands.clear();
        let Some(config) = &self.config else { return };

        let mut wanted: Vec<(String, Command)> = Vec::new();
        if let Some(k) = &config.hotkeys.panel {
            wanted.push((k.clone(), Command::OpenPanel));
        }
        if let Some(k) = &config.hotkeys.freeze_all {
            wanted.push((k.clone(), Command::Env(EnvOp::FreezeAll)));
        }
        for (name, env) in &config.environments {
            if let Some(k) = &env.hotkey {
                wanted.push((k.clone(), Command::Env(EnvOp::Switch(name.clone()))));
            }
        }

        let mut errors = Vec::new();
        for (text, command) in wanted {
            let hotkey = match HotKey::from_str(&text) {
                Ok(h) => h,
                Err(e) => {
                    errors.push(format!("{text}：格式错误（{e}）"));
                    continue;
                }
            };
            match self.hotkey_manager.register(hotkey) {
                Ok(()) => {
                    self.registered.push(hotkey);
                    self.hotkey_commands.insert(hotkey.id(), command);
                }
                Err(e) => errors.push(format!("{text}：注册失败，可能已被其他程序或本配置中的其他项占用（{e}）")),
            }
        }
        info!(count = self.registered.len(), "已注册全局快捷键");
        if !errors.is_empty() {
            warn!("快捷键问题：{}", errors.join("；"));
            let _ = notify_rust::Notification::new().summary("envfreeze 快捷键问题").body(&errors.join("\n")).show();
        }
    }

    /// 读取各环境状态；状态或配置变化时重建菜单、图标和提示。
    fn refresh(&mut self, force: bool) {
        let states: Vec<EnvView> = match &self.config {
            Some(config) => config
                .environments
                .keys()
                .map(|name| EnvView {
                    name: name.clone(),
                    state: status::env_state(name).unwrap_or(EnvState::Stopped),
                    hidden_windows: status::hidden_windows(name).unwrap_or_default(),
                })
                .collect(),
            None => Vec::new(),
        };
        if !force && states == self.states {
            self.update_tooltip();
            return;
        }
        self.states = states;
        let only_states: Vec<EnvState> = self.states.iter().map(|s| s.state).collect();
        let _ = self.icon.set_icon(Some(icon::render(icon::color_for(&only_states))));
        let menu = self.build_menu();
        self.icon.set_menu(Some(Box::new(menu)));
        self.update_tooltip();
    }

    fn update_tooltip(&self) {
        let names = |wanted: EnvState| -> String {
            let v: Vec<&str> = self.states.iter().filter(|s| s.state == wanted).map(|s| s.name.as_str()).collect();
            if v.is_empty() { "无".into() } else { v.join(", ") }
        };
        let mut tip = format!("envfreeze\n运行中：{}\n已冻结：{}", names(EnvState::Running), names(EnvState::Frozen));
        if let Some(busy) = &self.busy {
            tip = format!("envfreeze\n正在{busy}…");
        }
        // 托盘提示最多 127 个字符。
        let tip: String = tip.chars().take(120).collect();
        let _ = self.icon.set_tooltip(Some(tip));
    }

    fn build_menu(&mut self) -> Menu {
        self.menu_commands.clear();
        let menu = Menu::new();
        let hotkey_hint = |k: Option<&String>| k.map(|k| format!("\t{k}")).unwrap_or_default();

        let active: Vec<&str> =
            self.states.iter().filter(|s| s.state == EnvState::Running).map(|s| s.name.as_str()).collect();
        let header = if active.is_empty() {
            "envfreeze".to_string()
        } else {
            format!("envfreeze — 当前：{}", active.join(", "))
        };
        append(&menu, &MenuItem::new(header, false, None));
        append(&menu, &PredefinedMenuItem::separator());

        if let Some(err) = &self.config_error {
            let short: String = err.chars().take(60).collect();
            append(&menu, &MenuItem::new(format!("配置有误：{short}"), false, None));
        }
        let envs: Vec<(EnvView, Option<String>)> = self
            .states
            .iter()
            .map(|view| {
                let env = self.config.as_ref().and_then(|c| c.environments.get(&view.name));
                (view.clone(), env.and_then(|e| e.hotkey.clone()))
            })
            .collect();
        if envs.is_empty() {
            append(&menu, &MenuItem::new("（配置中还没有环境）", false, None));
        }
        for (EnvView { name, state, hidden_windows }, hotkey) in envs {
            let label = if hidden_windows.is_empty() {
                format!("{name}（{}）", state.label())
            } else {
                format!("{name}（{} · {} 个窗口）", state.label(), hidden_windows.len())
            };
            let sub = Submenu::new(label, true);
            let items = [
                (format!("切换到此环境{}", hotkey_hint(hotkey.as_ref())), EnvOp::Switch(name.clone()), true),
                ("启动".into(), EnvOp::Launch(name.clone()), state == EnvState::Stopped),
                ("冻结".into(), EnvOp::Freeze(name.clone()), state == EnvState::Running),
                ("恢复".into(), EnvOp::Thaw(name.clone()), matches!(state, EnvState::Frozen | EnvState::Orphaned)),
                ("关闭环境".into(), EnvOp::Stop(name.clone()), matches!(state, EnvState::Running | EnvState::Frozen)),
            ];
            for (text, op, enabled) in items {
                let item = self.item(text, Command::Env(op), enabled);
                let _ = sub.append(&item);
            }
            if !hidden_windows.is_empty() {
                // 只做展示：让人知道这个环境里藏着哪些窗口。
                let _ = sub.append(&PredefinedMenuItem::separator());
                let _ = sub.append(&MenuItem::new("隐藏的窗口：", false, None));
                for title in hidden_windows.iter().take(MAX_LISTED_WINDOWS) {
                    let title: String = title.chars().take(60).collect();
                    let _ = sub.append(&MenuItem::new(format!("    {title}"), false, None));
                }
                if hidden_windows.len() > MAX_LISTED_WINDOWS {
                    let more = format!("    …还有 {} 个", hidden_windows.len() - MAX_LISTED_WINDOWS);
                    let _ = sub.append(&MenuItem::new(more, false, None));
                }
            }
            append(&menu, &sub);
        }

        append(&menu, &PredefinedMenuItem::separator());
        let hotkeys = self.config.as_ref().map(|c| (c.hotkeys.freeze_all.clone(), c.hotkeys.panel.clone()));
        let (freeze_all_key, panel_key) = hotkeys.unwrap_or_default();
        let freeze_all = self.item(
            format!("冻结全部{}", hotkey_hint(freeze_all_key.as_ref())),
            Command::Env(EnvOp::FreezeAll),
            true,
        );
        append(&menu, &freeze_all);
        let panel = self.item(format!("打开面板{}", hotkey_hint(panel_key.as_ref())), Command::OpenPanel, true);
        append(&menu, &panel);
        let edit = self.item("编辑配置文件".into(), Command::EditConfig, true);
        append(&menu, &edit);
        let logs = self.item("打开日志目录".into(), Command::OpenLogs, true);
        append(&menu, &logs);
        let autostart = CheckMenuItem::new("开机自启", true, shell::autostart_enabled(), None);
        self.menu_commands.insert(autostart.id().clone(), Command::ToggleAutostart);
        append(&menu, &autostart);
        self.autostart_item = Some(autostart);
        append(&menu, &PredefinedMenuItem::separator());
        let quit = self.item("退出托盘".into(), Command::Quit, true);
        append(&menu, &quit);
        menu
    }

    fn item(&mut self, text: String, command: Command, enabled: bool) -> MenuItem {
        let item = MenuItem::new(text, enabled, None);
        self.menu_commands.insert(item.id().clone(), command);
        item
    }

    /// 处理托盘、菜单、快捷键事件。返回 false 表示退出。
    fn pump(&mut self) -> bool {
        let mut commands = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            commands.extend(self.menu_commands.get(&event.id).cloned());
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                commands.push(Command::OpenPanel);
            }
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            debug!(id = event.id(), state = ?event.state(), "快捷键事件");
            if event.state() == HotKeyState::Pressed {
                commands.extend(self.hotkey_commands.get(&event.id()).cloned());
            }
        }
        for command in commands {
            if !self.execute(command) {
                return false;
            }
        }
        true
    }

    fn execute(&mut self, command: Command) -> bool {
        match command {
            Command::Env(op) => {
                self.busy = Some(op.describe());
                self.update_tooltip();
                let _ = self.worker.send(op);
            }
            Command::OpenPanel => self.open_panel(),
            Command::EditConfig => match config::ensure_exists(&self.config_path) {
                Ok(_) => shell::open(&self.config_path),
                Err(e) => shell::error_box(&format!("{e:#}")),
            },
            Command::OpenLogs => {
                if let Ok(dir) = paths::log_dir() {
                    shell::open(&dir);
                }
            }
            Command::ToggleAutostart => {
                let enable = !shell::autostart_enabled();
                if let Err(e) = shell::set_autostart(enable) {
                    shell::error_box(&format!("{e:#}"));
                }
                if let Some(item) = &self.autostart_item {
                    item.set_checked(shell::autostart_enabled());
                }
            }
            Command::Quit => return false,
        }
        true
    }

    fn open_panel(&mut self) {
        if let Some(child) = &mut self.panel
            && matches!(child.try_wait(), Ok(None))
        {
            shell::allow_foreground(child.id());
            if shell::focus_window(panel::TITLE) {
                return;
            }
        }
        match spawn_panel(&self.config_path) {
            Ok(child) => self.panel = Some(child),
            Err(e) => shell::error_box(&format!("{e:#}")),
        }
    }
}

fn append(menu: &Menu, item: &dyn IsMenuItem) {
    if let Err(e) = menu.append(item) {
        warn!("添加菜单项失败：{e}");
    }
}
