//! envfreeze：按"项目环境"成套冻结和恢复 Windows 上的开发程序。

mod config;
mod freeze;
mod guard;
mod holder;
mod job;
mod launch;
mod memory;
mod ntdll;
mod paths;
mod process;
mod state;
mod status;
mod win;

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "envfreeze", version, about = "按项目环境成套冻结/恢复 Windows 开发程序")]
struct Cli {
    /// 配置文件路径（默认读取环境变量 ENVFREEZE_CONFIG，否则 %APPDATA%\envfreeze\config.toml）
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// 输出调试日志（也可用 RUST_LOG 精细控制）
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动环境中的所有程序
    Launch { env: String },
    /// 挂起环境中的所有进程并把它们的内存换出
    Freeze { env: String },
    /// 恢复环境中的所有进程
    Thaw { env: String },
    /// 列出各环境状态；指定环境名时列出进程明细
    Status { env: Option<String> },
    /// 内部命令：持有环境 Job 的后台进程
    #[command(name = "__hold", hide = true)]
    Hold { env: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("错误：{e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<bool> {
    let env_name = match &cli.command {
        Command::Launch { env } | Command::Freeze { env } | Command::Thaw { env } | Command::Hold { env } => Some(env),
        Command::Status { env } => env.as_ref(),
    };
    if let Some(env) = env_name {
        config::validate_env_name(env)?;
    }
    init_tracing(&cli)?;

    let config_path = match cli.config {
        Some(path) => path,
        None => paths::default_config()?,
    };
    match &cli.command {
        Command::Launch { env } => launch::run(&config_path, env),
        Command::Freeze { env } => freeze::freeze(env),
        Command::Thaw { env } => freeze::thaw(env),
        Command::Status { env } => status::run(&config_path, env.as_deref()).map(|()| true),
        Command::Hold { env } => holder::run(&config_path, env).map(|()| true),
    }
}

/// CLI 的日志写到 stderr；holder 没有控制台，日志追加到 `%LOCALAPPDATA%\envfreeze\logs\<环境名>.log`。
fn init_tracing(cli: &Cli) -> Result<()> {
    let default_level = match (&cli.command, cli.verbose) {
        (_, true) | (Command::Hold { .. }, _) => "envfreeze=debug",
        _ => "envfreeze=warn",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);

    if let Command::Hold { env } = &cli.command {
        let path = paths::holder_log(env)?;
        std::fs::create_dir_all(path.parent().expect("日志文件必有父目录"))?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        builder.with_ansi(false).with_writer(Mutex::new(file)).init();
    } else {
        builder.with_target(false).without_time().with_writer(std::io::stderr).init();
    }
    Ok(())
}
