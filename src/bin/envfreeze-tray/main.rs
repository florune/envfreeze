//! envfreeze 托盘程序。
//!
//! 同一个 exe 有三种运行方式：
//! - 无参数：常驻托盘，注册全局快捷键（只用 Win32 消息循环，几乎不占内存）；
//! - `--panel`：egui 面板，由托盘按需启动，关闭即退出，不常驻；
//! - `__hold <环境名>`：环境的 holder 进程（与命令行版相同）。
//!
//! 所有 `--config <路径>` 参数都会透传，默认使用 `%APPDATA%\envfreeze\config.toml`。

#![windows_subsystem = "windows"]

mod apps;
mod fonts;
mod icon;
mod ops;
mod panel;
mod picker;
mod shell;
mod tray;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use envfreeze::{holder, paths};

enum Mode {
    Tray,
    Panel,
    Hold(String),
}

fn main() {
    let result = parse_args().and_then(|(mode, config_path)| match mode {
        Mode::Hold(env) => holder::main(&config_path, &env),
        Mode::Panel => {
            init_logging("panel")?;
            panel::run(config_path)
        }
        Mode::Tray => {
            init_logging("tray")?;
            tray::run(config_path)
        }
    });
    if let Err(e) = result {
        tracing::error!("{e:#}");
        shell::error_box(&format!("{e:#}"));
    }
}

fn parse_args() -> Result<(Mode, PathBuf)> {
    let mut mode = Mode::Tray;
    let mut config = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = Some(PathBuf::from(args.next().context("--config 缺少路径")?)),
            "--panel" => mode = Mode::Panel,
            "__hold" => mode = Mode::Hold(args.next().context("__hold 缺少环境名")?),
            other => bail!("未知参数 `{other}`"),
        }
    }
    let config = match config {
        Some(path) => path,
        None => paths::default_config()?,
    };
    Ok((mode, config))
}

/// 托盘和面板没有控制台，日志写到 `%LOCALAPPDATA%\envfreeze\logs\<name>.log`。
fn init_logging(name: &str) -> Result<()> {
    let dir = paths::log_dir()?;
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{name}.log")))?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("envfreeze=info,envfreeze_tray=info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    Ok(())
}
