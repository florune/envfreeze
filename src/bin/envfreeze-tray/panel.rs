//! egui 面板：查看/操作环境、配置快捷键和白名单、开机自启。
//!
//! 面板是独立进程，由托盘按需启动，关闭即退出，不常驻内存。
//! 修改直接写回配置文件，托盘检测到文件变化后会自动重新注册快捷键。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow};
use eframe::egui::{self, Color32, Key, Modifiers, RichText};
use envfreeze::config::{self, Config};
use envfreeze::config_edit::{self, GlobalHotkey};
use envfreeze::memory::fmt_bytes;
use envfreeze::paths;
use envfreeze::status::{self, EnvState, EnvStatus};
use global_hotkey::hotkey::HotKey;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

use crate::ops::{self, EnvOp};
use crate::picker::{self, Picker};
use crate::{fonts, shell};

pub const TITLE: &str = "envfreeze 面板";
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub fn run(config_path: PathBuf) -> Result<()> {
    let _mutex = unsafe { CreateMutexW(None, false, w!("Local\\envfreeze_panel")) }?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        shell::focus_window(TITLE);
        return Ok(());
    }
    config::ensure_exists(&config_path)?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([860.0, 620.0])
            .with_min_inner_size([620.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            fonts::install(&cc.egui_ctx);
            Ok(Box::new(PanelApp::new(config_path, cc.egui_ctx.clone())))
        }),
    )
    .map_err(|e| anyhow!("面板运行失败：{e}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Envs,
    Hotkeys,
    Exclude,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HotkeyTarget {
    Global(GlobalHotkey),
    Env(String),
}

struct LogEntry {
    time: String,
    ok: bool,
    title: String,
    body: String,
}

/// 后台操作与界面共享的状态。
#[derive(Default)]
struct Shared {
    busy: Option<String>,
    log: Vec<LogEntry>,
    /// 操作完成后需要立即刷新状态。
    dirty: bool,
}

struct PanelApp {
    config_path: PathBuf,
    ctx: egui::Context,
    config: Option<Config>,
    config_error: Option<String>,
    config_mtime: Option<SystemTime>,
    statuses: Vec<EnvStatus>,
    last_refresh: Option<Instant>,
    tab: Tab,
    shared: Arc<Mutex<Shared>>,
    recording: Option<HotkeyTarget>,
    /// 最近一次编辑操作的反馈：(是否成功, 文字)。
    notice: Option<(bool, String)>,
    exclude_global: String,
    exclude_env: BTreeMap<String, String>,
    autostart: bool,
    picker: Option<Picker>,
    new_env: String,
    /// 等待二次确认删除的环境。
    confirm_delete: Option<String>,
}

/// 环境页上对配置的修改，在界面绘制完后统一执行。
enum EnvEdit {
    AddEnv(String),
    RemoveEnv(String),
    RemoveProgram(String, usize),
}

impl PanelApp {
    fn new(config_path: PathBuf, ctx: egui::Context) -> Self {
        let mut app = Self {
            config_path,
            ctx,
            config: None,
            config_error: None,
            config_mtime: None,
            statuses: Vec::new(),
            last_refresh: None,
            tab: Tab::Envs,
            shared: Arc::default(),
            recording: None,
            notice: None,
            exclude_global: String::new(),
            exclude_env: BTreeMap::new(),
            autostart: shell::autostart_enabled(),
            picker: None,
            new_env: String::new(),
            confirm_delete: None,
        };
        app.refresh(true);
        app
    }

    /// 重新读取配置（文件有变化或 force）和各环境状态。
    fn refresh(&mut self, force_config: bool) {
        let mtime = std::fs::metadata(&self.config_path).and_then(|m| m.modified()).ok();
        if force_config || mtime != self.config_mtime {
            self.config_mtime = mtime;
            match Config::load(&self.config_path) {
                Ok(config) => {
                    self.exclude_global = config.exclude.join("\n");
                    self.exclude_env =
                        config.environments.iter().map(|(name, env)| (name.clone(), env.exclude.join("\n"))).collect();
                    self.config = Some(config);
                    self.config_error = None;
                }
                Err(e) => self.config_error = Some(format!("{e:#}")),
            }
        }
        self.statuses = match &self.config {
            Some(config) => status::all(config).unwrap_or_default(),
            None => Vec::new(),
        };
        self.last_refresh = Some(Instant::now());
    }

    fn busy(&self) -> Option<String> {
        self.shared.lock().unwrap().busy.clone()
    }

    fn run_op(&self, op: EnvOp) {
        let mut shared = self.shared.lock().unwrap();
        if shared.busy.is_some() {
            return;
        }
        shared.busy = Some(op.describe());
        drop(shared);
        let (config_path, shared, ctx) = (self.config_path.clone(), self.shared.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = ops::execute(&config_path, &op);
            let mut shared = shared.lock().unwrap();
            shared.log.push(LogEntry {
                time: shell::local_time(),
                ok: result.ok,
                title: result.title,
                body: result.body,
            });
            shared.busy = None;
            shared.dirty = true;
            ctx.request_repaint();
        });
    }

    /// 执行一次配置修改并给出反馈。
    fn edit(&mut self, what: &str, f: impl FnOnce(&std::path::Path) -> Result<()>) {
        self.notice = Some(match f(&self.config_path) {
            Ok(()) => (true, format!("{what}：已保存，托盘会在 2 秒内生效")),
            Err(e) => (false, format!("{what}失败：{e:#}")),
        });
        self.refresh(true);
    }

    fn capture_hotkey(&mut self, ctx: &egui::Context) {
        let Some(target) = self.recording.clone() else { return };
        // 按下修饰键本身也会产生按键事件，要等到第一个可作为主键的按键（或 Esc）。
        let pressed = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } if *key == Key::Escape || is_main_key(*key) => {
                    Some((*key, *modifiers))
                }
                _ => None,
            })
        });
        let Some((key, modifiers)) = pressed else { return };
        if key == Key::Escape {
            self.recording = None;
            return;
        }
        let text = match hotkey_text(key, modifiers) {
            Ok(text) => text,
            Err(msg) => {
                self.notice = Some((false, msg));
                return;
            }
        };
        self.recording = None;
        match target {
            HotkeyTarget::Global(which) => {
                self.edit("快捷键", |p| config_edit::set_global_hotkey(p, which, Some(&text)));
            }
            HotkeyTarget::Env(env) => self.edit("快捷键", |p| config_edit::set_env_hotkey(p, &env, Some(&text))),
        }
    }
}

/// 能作为快捷键主键的按键：字母、数字、F1–F24、空格。
fn is_main_key(key: Key) -> bool {
    let name = key.name();
    let single_alnum = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphanumeric());
    single_alnum || is_function_key(key) || key == Key::Space
}

fn is_function_key(key: Key) -> bool {
    let name = key.name();
    name.len() > 1 && name.starts_with('F') && name[1..].parse::<u8>().is_ok()
}

/// 把 egui 的按键转成配置里的快捷键文本，并用 global-hotkey 校验。
fn hotkey_text(key: Key, modifiers: Modifiers) -> Result<String, String> {
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if parts.is_empty() && !is_function_key(key) {
        return Err("快捷键至少需要一个修饰键（Ctrl / Alt / Shift），F1–F24 除外".into());
    }
    parts.push(key.name());
    let text = parts.join("+");
    HotKey::from_str(&text).map_err(|e| format!("不支持的快捷键 {text}：{e}"))?;
    Ok(text)
}

fn state_color(state: EnvState) -> Color32 {
    match state {
        EnvState::Running => Color32::from_rgb(46, 170, 92),
        EnvState::Frozen => Color32::from_rgb(56, 152, 236),
        EnvState::Orphaned => Color32::from_rgb(230, 140, 40),
        EnvState::Stopped => Color32::GRAY,
    }
}

impl eframe::App for PanelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dirty = std::mem::take(&mut self.shared.lock().unwrap().dirty);
        if dirty || self.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL) {
            self.refresh(false);
        }
        // 面板打开时内存占用较高（主要是图形驱动），Esc 随手关掉；录制快捷键时 Esc 用来取消录制。
        if self.recording.is_none() && self.picker.is_none() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.capture_hotkey(&ctx);

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::Envs, "环境"),
                    (Tab::Hotkeys, "快捷键"),
                    (Tab::Exclude, "白名单"),
                    (Tab::Settings, "设置与日志"),
                ] {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
                if let Some(busy) = self.busy() {
                    ui.separator();
                    ui.spinner();
                    ui.label(format!("正在{busy}…"));
                }
            });
            ui.add_space(4.0);
        });
        egui::Panel::bottom("notice").show(ui, |ui| {
            ui.add_space(2.0);
            match &self.notice {
                Some((true, text)) => ui.label(RichText::new(text).color(Color32::from_rgb(46, 170, 92))),
                Some((false, text)) => ui.label(RichText::new(text).color(Color32::from_rgb(220, 70, 70))),
                None => ui.add(
                    egui::Label::new(RichText::new(format!("配置文件：{}", self.config_path.display())).weak())
                        .truncate(),
                ),
            };
            ui.add_space(2.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = &self.config_error {
                ui.colored_label(
                    Color32::from_rgb(220, 70, 70),
                    format!("配置文件有误，正在使用上一次的有效配置：{err}"),
                );
                ui.separator();
            }
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.tab {
                Tab::Envs => self.envs_tab(ui),
                Tab::Hotkeys => self.hotkeys_tab(ui),
                Tab::Exclude => self.exclude_tab(ui),
                Tab::Settings => self.settings_tab(ui),
            });
        });

        if let Some(picker) = &mut self.picker {
            match picker.show(&ctx, &self.config_path) {
                picker::Outcome::Open => {}
                picker::Outcome::Cancelled => self.picker = None,
                picker::Outcome::Added(message) => {
                    self.picker = None;
                    self.notice = Some((true, message));
                    self.refresh(true);
                }
            }
        }

        ctx.request_repaint_after(REFRESH_INTERVAL);
    }
}

impl PanelApp {
    fn envs_tab(&mut self, ui: &mut egui::Ui) {
        let mut edit: Option<EnvEdit> = None;
        let mut open_picker: Option<String> = None;
        ui.horizontal(|ui| {
            ui.label("新建环境");
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.new_env)
                    .hint_text("例如 proj-a（字母、数字、- _ .）")
                    .desired_width(240.0),
            );
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.button("创建").clicked() || enter) && !self.new_env.trim().is_empty() {
                edit = Some(EnvEdit::AddEnv(self.new_env.trim().to_owned()));
            }
        });
        ui.add_space(6.0);
        if self.statuses.is_empty() {
            ui.label("还没有环境。先在上面新建一个，再往里面添加程序。");
        }

        let busy = self.busy().is_some();
        let mut op = None;
        let mut toggle: Option<(String, String, bool)> = None;
        for s in &self.statuses {
            let env_config = self.config.as_ref().and_then(|c| c.environments.get(&s.name));
            ui.group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.heading(&s.name);
                    ui.label(RichText::new(s.state.label()).color(state_color(s.state)).strong());
                    let total = s.total();
                    ui.weak(format!(
                        "{} 个进程 · 工作集 {} · 私有提交 {}",
                        s.processes.len(),
                        fmt_bytes(total.working_set),
                        fmt_bytes(total.private)
                    ));
                    if let Some(k) = env_config.and_then(|e| e.hotkey.as_ref()) {
                        ui.weak(format!("· {k}"));
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("切换到此环境").on_hover_text("冻结其他运行中的环境，再启动或恢复本环境").clicked()
                        {
                            op = Some(EnvOp::Switch(s.name.clone()));
                        }
                        if ui.add_enabled(s.state == EnvState::Stopped, egui::Button::new("启动")).clicked() {
                            op = Some(EnvOp::Launch(s.name.clone()));
                        }
                        if ui.add_enabled(s.state == EnvState::Running, egui::Button::new("冻结")).clicked() {
                            op = Some(EnvOp::Freeze(s.name.clone()));
                        }
                        let thawable = matches!(s.state, EnvState::Frozen | EnvState::Orphaned);
                        if ui.add_enabled(thawable, egui::Button::new("恢复")).clicked() {
                            op = Some(EnvOp::Thaw(s.name.clone()));
                        }
                    });
                });

                let programs = env_config.map(|e| e.programs.as_slice()).unwrap_or_default();
                let stopped = s.state == EnvState::Stopped;
                egui::CollapsingHeader::new(format!("程序（{}）", programs.len()))
                    .id_salt(("programs", &s.name))
                    .default_open(programs.is_empty())
                    .show(ui, |ui| {
                        if programs.is_empty() {
                            ui.weak("还没有程序。点「添加程序」从电脑上已安装或正在运行的程序里挑选。");
                        }
                        for (i, program) in programs.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let remove = ui
                                    .add_enabled(stopped, egui::Button::new("删除").small())
                                    .on_disabled_hover_text("环境运行中，先关闭环境里的程序再删除");
                                if remove.clicked() {
                                    edit = Some(EnvEdit::RemoveProgram(s.name.clone(), i));
                                }
                                ui.strong(program.display_name());
                                let mut detail = picker::short_path(&program.path);
                                if !program.args.is_empty() {
                                    detail += &format!(" {}", program.args.join(" "));
                                }
                                ui.add(egui::Label::new(RichText::new(detail).weak()).truncate())
                                    .on_hover_text(&program.path);
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui.button("＋ 添加程序").clicked() {
                                open_picker = Some(s.name.clone());
                            }
                            if !stopped {
                                ui.weak("（新添加的程序在下次启动环境时生效）");
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if self.confirm_delete.as_deref() == Some(s.name.as_str()) {
                                    if ui.button("取消").clicked() {
                                        self.confirm_delete = None;
                                    }
                                    if ui
                                        .button(RichText::new("确认删除").color(Color32::from_rgb(220, 70, 70)))
                                        .clicked()
                                    {
                                        edit = Some(EnvEdit::RemoveEnv(s.name.clone()));
                                    }
                                } else if ui
                                    .add_enabled(stopped, egui::Button::new("删除环境"))
                                    .on_disabled_hover_text("只能删除未运行的环境")
                                    .clicked()
                                {
                                    self.confirm_delete = Some(s.name.clone());
                                }
                            });
                        });
                    });

                if s.processes.is_empty() {
                    return;
                }
                egui::CollapsingHeader::new("进程明细").id_salt(&s.name).show(ui, |ui| {
                    ui.weak("勾选「冻结时跳过」的进程在冻结本环境时保持运行（下次冻结生效）。");
                    let own: Vec<String> =
                        env_config.map(|e| e.exclude.iter().map(|p| p.to_lowercase()).collect()).unwrap_or_default();
                    egui::Grid::new(("procs", &s.name)).striped(true).num_columns(5).show(ui, |ui| {
                        ui.strong("冻结时跳过");
                        ui.strong("PID");
                        ui.strong("进程");
                        ui.strong("工作集");
                        ui.strong("私有提交");
                        ui.end_row();
                        for p in &s.processes {
                            let mut checked = p.excluded_by.is_some();
                            // 本环境里不带通配符的规则可以在这里直接取消；通配符或全局规则去白名单页改。
                            let editable = match &p.excluded_by {
                                None => true,
                                Some(rule) => own.contains(rule) && !rule.contains(['*', '?']),
                            };
                            let response = ui.add_enabled(editable, egui::Checkbox::without_text(&mut checked));
                            let response = match &p.excluded_by {
                                Some(rule) if !editable => {
                                    response.on_disabled_hover_text(format!("由规则 `{rule}` 匹配，请在白名单页修改"))
                                }
                                _ => response,
                            };
                            if response.changed() {
                                // 勾选时按进程名添加规则；取消时删除命中它的那条规则。
                                let rule = p.excluded_by.clone().unwrap_or_else(|| p.name.to_lowercase());
                                toggle = Some((s.name.clone(), rule, checked));
                            }
                            ui.label(p.pid.to_string());
                            ui.label(&p.name);
                            ui.label(fmt_bytes(p.memory.working_set));
                            ui.label(fmt_bytes(p.memory.private));
                            ui.end_row();
                        }
                    });
                });
            });
            ui.add_space(6.0);
        }

        if let Some(op) = op {
            self.run_op(op);
        }
        if let Some(env) = open_picker {
            self.picker = Some(Picker::new(&env, ui.ctx()));
        }
        match edit {
            Some(EnvEdit::AddEnv(env)) => {
                self.edit(&format!("新建环境 {env}"), |p| config_edit::add_env(p, &env));
                if matches!(self.notice, Some((true, _))) {
                    self.new_env.clear();
                }
            }
            Some(EnvEdit::RemoveEnv(env)) => {
                self.confirm_delete = None;
                self.edit(&format!("删除环境 {env}"), |p| config_edit::remove_env(p, &env));
            }
            Some(EnvEdit::RemoveProgram(env, i)) => {
                self.edit("删除程序", |p| config_edit::remove_program(p, &env, i));
            }
            None => {}
        }
        if let Some((env, rule, exclude)) = toggle {
            let mut list: Vec<String> = self
                .config
                .as_ref()
                .and_then(|c| c.environments.get(&env))
                .map(|e| e.exclude.clone())
                .unwrap_or_default();
            list.retain(|p| p.to_lowercase() != rule);
            if exclude {
                list.push(rule);
            }
            self.edit(&format!("{env} 白名单"), |p| config_edit::set_exclude(p, Some(&env), &list));
        }
    }

    fn hotkeys_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("点「录制」后按下组合键即可保存，Esc 取消。快捷键由托盘注册，托盘需要保持运行。");
        ui.add_space(6.0);
        let Some(config) = &self.config else { return };
        let mut rows: Vec<(String, HotkeyTarget, Option<String>)> = vec![
            ("打开面板".into(), HotkeyTarget::Global(GlobalHotkey::Panel), config.hotkeys.panel.clone()),
            ("冻结全部环境".into(), HotkeyTarget::Global(GlobalHotkey::FreezeAll), config.hotkeys.freeze_all.clone()),
        ];
        for (name, env) in &config.environments {
            rows.push((format!("切换到 {name}"), HotkeyTarget::Env(name.clone()), env.hotkey.clone()));
        }

        let mut action: Option<(HotkeyTarget, bool)> = None; // (目标, true=录制 / false=清除)
        egui::Grid::new("hotkeys").striped(true).num_columns(4).spacing([16.0, 8.0]).show(ui, |ui| {
            for (label, target, current) in rows {
                ui.label(label);
                let recording = self.recording.as_ref() == Some(&target);
                if recording {
                    ui.label(RichText::new("请按下组合键…").color(Color32::from_rgb(56, 152, 236)));
                } else {
                    ui.monospace(current.as_deref().unwrap_or("—"));
                }
                if ui.button(if recording { "取消" } else { "录制" }).clicked() {
                    action = Some((target.clone(), !recording));
                }
                if ui.add_enabled(current.is_some(), egui::Button::new("清除")).clicked() {
                    action = Some((target, false));
                }
                ui.end_row();
            }
        });

        match action {
            Some((target, true)) => self.recording = Some(target),
            Some((target, false)) => {
                if self.recording.as_ref() == Some(&target) {
                    self.recording = None;
                    return;
                }
                match target {
                    HotkeyTarget::Global(which) => {
                        self.edit("清除快捷键", |p| config_edit::set_global_hotkey(p, which, None));
                    }
                    HotkeyTarget::Env(env) => {
                        self.edit("清除快捷键", |p| config_edit::set_env_hotkey(p, &env, None))
                    }
                }
            }
            None => {}
        }
    }

    fn exclude_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("冻结时跳过这些进程，让它们保持运行。每行一条规则，按进程名匹配，不区分大小写。");
        ui.label("支持 * 和 ? 通配符；不写扩展名也能匹配 .exe，例如 navicat、node*、Claude*.exe。");
        ui.weak("系统进程、explorer.exe、安全软件和 envfreeze 自身始终不会被冻结，无需添加。");
        ui.add_space(8.0);

        let mut save: Option<Option<String>> = None; // Some(None)=全局，Some(Some(env))=环境
        ui.strong("全局（所有环境）");
        ui.add(egui::TextEdit::multiline(&mut self.exclude_global).desired_rows(3).desired_width(f32::INFINITY));
        if ui.button("保存全局白名单").clicked() {
            save = Some(None);
        }
        for (env, text) in self.exclude_env.iter_mut() {
            ui.add_space(10.0);
            ui.strong(format!("环境 {env}"));
            ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY));
            if ui.button(format!("保存 {env} 白名单")).clicked() {
                save = Some(Some(env.clone()));
            }
        }

        if let Some(target) = save {
            let text = match &target {
                Some(env) => self.exclude_env.get(env).cloned().unwrap_or_default(),
                None => self.exclude_global.clone(),
            };
            let list: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            self.edit("白名单", |p| config_edit::set_exclude(p, target.as_deref(), &list));
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        if ui.checkbox(&mut self.autostart, "开机自动启动托盘").changed() {
            if let Err(e) = shell::set_autostart(self.autostart) {
                self.notice = Some((false, format!("{e:#}")));
            }
            self.autostart = shell::autostart_enabled();
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("打开配置文件").on_hover_text("添加环境和程序请直接编辑配置文件").clicked()
            {
                shell::open(&self.config_path);
            }
            if ui.button("打开配置所在目录").clicked()
                && let Some(dir) = self.config_path.parent()
            {
                shell::open(dir);
            }
            if ui.button("打开日志目录").clicked()
                && let Ok(dir) = paths::log_dir()
            {
                shell::open(&dir);
            }
        });
        ui.weak(self.config_path.display().to_string());

        ui.add_space(12.0);
        ui.strong("本次面板的操作记录");
        let shared = self.shared.lock().unwrap();
        if shared.log.is_empty() {
            ui.weak("（暂无）");
        }
        for entry in shared.log.iter().rev() {
            ui.horizontal(|ui| {
                ui.weak(&entry.time);
                let color = if entry.ok { Color32::from_rgb(46, 170, 92) } else { Color32::from_rgb(220, 70, 70) };
                ui.label(RichText::new(&entry.title).color(color).strong());
            });
            ui.label(&entry.body);
            ui.add_space(4.0);
        }
    }
}
