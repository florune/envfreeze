//! 面板里的"添加程序"对话框：从正在运行的程序、开始菜单、常用终端中挑选，
//! 或者浏览选择 exe，填好参数和工作目录后写入配置。
//! 选中 JetBrains IDE 时可一键为该环境创建独立实例。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use envfreeze::config_edit::{self, NewProgram};
use envfreeze::jetbrains::{self, JetBrainsIde};

use crate::apps::{self, AppEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Running,
    StartMenu,
    Terminals,
}

pub enum Outcome {
    /// 对话框继续显示。
    Open,
    Cancelled,
    /// 已添加，附带给用户的提示。
    Added(String),
}

pub struct Picker {
    pub env: String,
    source: Source,
    search: String,
    running: Vec<AppEntry>,
    terminals: Vec<AppEntry>,
    /// 开始菜单扫描较慢（要逐个解析快捷方式），在后台线程完成。
    start_menu: Arc<Mutex<Option<Vec<AppEntry>>>>,
    form: Option<Form>,
}

struct Form {
    name: String,
    path: String,
    /// 每行一个参数。
    args: String,
    cwd: String,
    ide: Option<JetBrainsIde>,
    isolate: bool,
    copy_settings: bool,
    error: Option<String>,
}

impl Picker {
    pub fn new(env: &str, ctx: &egui::Context) -> Self {
        let start_menu = Arc::new(Mutex::new(None));
        let (slot, ctx) = (start_menu.clone(), ctx.clone());
        std::thread::spawn(move || {
            let apps = apps::start_menu_apps();
            *slot.lock().unwrap() = Some(apps);
            ctx.request_repaint();
        });
        Self {
            env: env.to_owned(),
            source: Source::Running,
            search: String::new(),
            running: apps::running_apps(),
            terminals: apps::terminal_apps(),
            start_menu,
            form: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, config_path: &Path) -> Outcome {
        let mut outcome = Outcome::Open;
        let response = egui::Modal::new(egui::Id::unique("program-picker")).show(ctx, |ui| {
            ui.set_width(680.0);
            ui.heading(format!("添加程序到 {}", self.env));
            ui.add_space(6.0);
            outcome = if self.form.is_some() { self.form_ui(ui, config_path) } else { self.list_ui(ui) };
        });
        if response.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancelled;
        }
        outcome
    }

    fn list_ui(&mut self, ui: &mut egui::Ui) -> Outcome {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.source, Source::Running, format!("正在运行（{}）", self.running.len()));
            let menu_label = match &*self.start_menu.lock().unwrap() {
                Some(apps) => format!("开始菜单（{}）", apps.len()),
                None => "开始菜单（扫描中…）".into(),
            };
            ui.selectable_value(&mut self.source, Source::StartMenu, menu_label);
            ui.selectable_value(&mut self.source, Source::Terminals, "终端");
            ui.separator();
            if ui.button("浏览 exe…").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("程序", &["exe"]).pick_file()
            {
                self.open_form(&AppEntry {
                    name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                    path,
                    args: Vec::new(),
                    cwd: None,
                    detail: String::new(),
                });
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("搜索名称或路径").desired_width(f32::INFINITY));
        ui.add_space(4.0);

        let needle = self.search.to_lowercase();
        let entries: Vec<AppEntry> = match self.source {
            Source::Running => self.running.clone(),
            Source::Terminals => self.terminals.clone(),
            Source::StartMenu => self.start_menu.lock().unwrap().clone().unwrap_or_default(),
        };
        let mut picked = None;
        egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, false]).show(ui, |ui| {
            if self.source == Source::StartMenu && self.start_menu.lock().unwrap().is_none() {
                ui.spinner();
            }
            // 两端对齐的布局让每一项占满整行，整行都可点击，文字左对齐。
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                for app in entries.iter().filter(|a| {
                    needle.is_empty()
                        || a.name.to_lowercase().contains(&needle)
                        || a.path.display().to_string().to_lowercase().contains(&needle)
                }) {
                    let mut text = egui::text::LayoutJob::default();
                    text.append(
                        &app.name,
                        0.0,
                        egui::TextFormat::simple(egui::FontId::proportional(15.0), ui.visuals().strong_text_color()),
                    );
                    text.append(
                        &format!("\n{}", app.path.display()),
                        0.0,
                        egui::TextFormat::simple(egui::FontId::proportional(12.0), ui.visuals().weak_text_color()),
                    );
                    let response = ui.selectable_label(false, text);
                    let response =
                        if app.detail.is_empty() { response } else { response.on_hover_text(app.detail.clone()) };
                    if response.clicked() {
                        picked = Some(app.clone());
                    }
                }
            });
        });
        if let Some(app) = picked {
            self.open_form(&app);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.weak("系统组件和应用商店（打包）应用无法加入环境，不在列表中。");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("取消").clicked() { Outcome::Cancelled } else { Outcome::Open }
            })
            .inner
        })
        .inner
    }

    fn open_form(&mut self, app: &AppEntry) {
        let ide = jetbrains::detect(&app.path);
        self.form = Some(Form {
            name: ide.as_ref().map(|i| i.name.clone()).unwrap_or_else(|| app.name.clone()),
            path: app.path.display().to_string(),
            args: app.args.join("\n"),
            cwd: app.cwd.clone().unwrap_or_default(),
            isolate: ide.is_some(),
            copy_settings: true,
            ide,
            error: None,
        });
    }

    fn is_running(&self, path: &str) -> bool {
        self.running.iter().any(|a| a.path.display().to_string().eq_ignore_ascii_case(path))
    }

    fn form_ui(&mut self, ui: &mut egui::Ui, config_path: &Path) -> Outcome {
        let running = self.form.as_ref().is_some_and(|f| self.is_running(&f.path));
        let env = self.env.clone();
        let form = self.form.as_mut().expect("调用方已确认表单存在");

        egui::Grid::new("program-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("名称");
            ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(f32::INFINITY));
            ui.end_row();

            ui.label("程序");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut form.path).desired_width(ui.available_width() - 70.0));
                if ui.button("浏览…").clicked()
                    && let Some(path) = rfd::FileDialog::new().add_filter("程序", &["exe"]).pick_file()
                {
                    form.path = path.display().to_string();
                    form.ide = jetbrains::detect(&path);
                }
            });
            ui.end_row();

            ui.label("参数");
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut form.args)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("每行一个参数"),
                );
                if ui.button("＋ 添加文件夹（例如要打开的项目目录）…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    if !form.args.is_empty() && !form.args.ends_with('\n') {
                        form.args.push('\n');
                    }
                    form.args.push_str(&dir.display().to_string());
                }
            });
            ui.end_row();

            ui.label("工作目录");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.cwd)
                        .desired_width(ui.available_width() - 70.0)
                        .hint_text("可选；终端建议设为项目目录"),
                );
                if ui.button("选择…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    form.cwd = dir.display().to_string();
                }
            });
            ui.end_row();
        });

        if let Some(ide) = &form.ide {
            ui.add_space(6.0);
            ui.checkbox(&mut form.isolate, format!("为 {env} 使用独立的 {} 实例（推荐）", ide.name)).on_hover_text(
                format!(
                    "{name} 是单实例程序。不隔离的话，它会把项目交给已经打开的 {name}，\
                     无法和其他环境分开冻结。隔离后本环境的 {name} 使用自己的设置和缓存目录，插件与原安装共用。",
                    name = ide.name
                ),
            );
            ui.add_enabled_ui(form.isolate, |ui| {
                ui.checkbox(&mut form.copy_settings, "首次创建时复制当前的设置（主题、快捷键、许可证等）");
            });
            ui.weak("参数里每行填一个项目目录，它们会在这个实例里以多个窗口打开。首次启动需要重新建立索引。");
        }
        // 独立实例不会和已运行的 IDE 合并，这条提醒只对未隔离的程序有意义。
        let isolated = form.ide.is_some() && form.isolate;
        if running && !isolated {
            ui.add_space(6.0);
            ui.colored_label(
                Color32::from_rgb(230, 140, 40),
                "这个程序现在正在运行。如果它是单实例程序（如 Navicat），启动环境前要先把它关掉，\
                 否则请求会被转交给已运行的实例，不会进入环境。",
            );
        }
        if let Some(err) = &form.error {
            ui.colored_label(Color32::from_rgb(220, 70, 70), err);
        }

        ui.add_space(8.0);
        let mut outcome = Outcome::Open;
        let mut back = false;
        ui.horizontal(|ui| {
            if ui.button(RichText::new("添加到环境").strong()).clicked() {
                match submit(form, &env, config_path) {
                    Ok(message) => outcome = Outcome::Added(message),
                    Err(e) => form.error = Some(format!("{e:#}")),
                }
            }
            if ui.button("返回列表").clicked() {
                back = true;
            }
            if ui.button("取消").clicked() {
                outcome = Outcome::Cancelled;
            }
        });
        if back {
            self.form = None;
        }
        outcome
    }
}

fn submit(form: &Form, env: &str, config_path: &Path) -> Result<String> {
    let path = form.path.trim();
    if path.is_empty() {
        anyhow::bail!("请选择程序");
    }
    let mut program = NewProgram {
        name: form.name.trim().to_owned(),
        path: path.to_owned(),
        args: form.args.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
        cwd: Some(form.cwd.trim().to_owned()).filter(|c| !c.is_empty()),
        env: Default::default(),
    };
    let mut message = format!("已把 {} 添加到 {env}", program.name);
    if let Some(ide) = form.ide.as_ref().filter(|_| form.isolate) {
        let isolation = jetbrains::isolate(ide, env, form.copy_settings)?;
        program.env.insert(ide.properties_env_var.clone(), isolation.properties.display().to_string());
        message += &format!("，并创建了独立的 {} 配置", ide.name);
        if isolation.copied_settings {
            message += "（已复制现有设置";
            if !isolation.skipped.is_empty() {
                message += &format!("，{} 个被占用的文件未复制", isolation.skipped.len());
            }
            message += "）";
        }
    }
    config_edit::add_program(config_path, env, &program)?;
    Ok(message)
}

/// 供环境列表显示程序时使用的简短路径。
pub fn short_path(path: &str) -> String {
    let p = PathBuf::from(path);
    match (p.parent().and_then(|d| d.file_name()), p.file_name()) {
        (Some(dir), Some(file)) => format!("…\\{}\\{}", dir.to_string_lossy(), file.to_string_lossy()),
        _ => path.to_owned(),
    }
}
