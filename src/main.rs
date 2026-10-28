//! envfreeze 命令行。

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use envfreeze::config::{self, Config};
use envfreeze::freeze::{Failure, FreezeReport, ThawReport};
use envfreeze::launch::{LaunchReport, Level};
use envfreeze::memory::{fmt_bytes, fmt_delta};
use envfreeze::switch::{self, Action, Batch};
use envfreeze::{freeze, holder, launch, paths, status};
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
    /// 切换到环境：冻结其他运行中的环境，再启动或恢复目标环境
    Switch { env: String },
    /// 冻结所有运行中的环境
    FreezeAll,
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
    let config_path = match cli.config {
        Some(path) => path,
        None => paths::default_config()?,
    };
    if let Command::Hold { env } = &cli.command {
        holder::main(&config_path, env)?;
        return Ok(true);
    }

    let default_level = if cli.verbose { "envfreeze=debug" } else { "envfreeze=warn" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    match &cli.command {
        Command::Launch { env } => {
            config::validate_env_name(env)?;
            let report = launch::launch(&config_path, env)?;
            print_launch(&report);
            Ok(report.ok())
        }
        Command::Freeze { env } => {
            config::validate_env_name(env)?;
            let config = Config::load(&config_path)?;
            let report = switch::freeze_env(&config, env)?;
            print_freeze(&report);
            Ok(report.ok())
        }
        Command::Thaw { env } => {
            config::validate_env_name(env)?;
            let report = freeze::thaw(env)?;
            print_thaw(&report);
            Ok(report.ok())
        }
        Command::Switch { env } => {
            config::validate_env_name(env)?;
            let batch = switch::switch(&config_path, env)?;
            print_batch(&batch);
            Ok(batch.ok())
        }
        Command::FreezeAll => {
            let batch = switch::freeze_all(&config_path)?;
            print_batch(&batch);
            Ok(batch.ok())
        }
        Command::Status { env } => {
            let config = Config::load(&config_path)?;
            match env {
                Some(env) => print_status_detail(&config, env)?,
                None => print_status(&config)?,
            }
            Ok(true)
        }
        Command::Hold { .. } => unreachable!("已在前面处理"),
    }
}

fn print_launch(report: &LaunchReport) {
    println!("启动环境 `{}`", report.env);
    for (level, msg) in &report.lines {
        let mark = match level {
            Level::Ok => "✓",
            Level::Warn => "!",
            Level::Error => "✗",
        };
        println!("  {mark} {msg}");
    }
}

fn print_freeze(r: &FreezeReport) {
    if r.already_frozen {
        println!("环境 `{}` 已处于冻结状态，跳过（重复挂起会导致需要多次恢复）", r.env);
        return;
    }
    println!("已冻结环境 `{}`：挂起 {} 个进程", r.env, r.frozen.len());
    if !r.frozen.is_empty() {
        println!("{:>7}  {:>11}  {:>11}  {:>11}  进程", "PID", "工作集(前)", "工作集(后)", "私有提交");
        for row in &r.frozen {
            println!(
                "{:>7}  {:>11}  {:>11}  {:>11}  {}",
                row.pid,
                fmt_bytes(row.before.working_set),
                fmt_bytes(row.after.working_set),
                fmt_bytes(row.after.private),
                row.name
            );
        }
        println!("  工作集共释放 {}", fmt_bytes(r.released()));
    }
    for s in &r.skipped {
        println!("  跳过 PID {} {}：{}", s.pid, s.name, s.reason);
    }
    if let (Some(b), Some(a)) = (r.available_before, r.available_after) {
        println!("系统可用物理内存：{} → {}（{}）", fmt_bytes(b), fmt_bytes(a), fmt_delta(b, a));
        println!("  注：被换出的页面会先进入修改列表/内存压缩，可用内存可能在之后几秒内继续上升");
    }
    println!("挂起用时 {} ms，清空工作集用时 {} ms", r.suspend_elapsed.as_millis(), r.trim_elapsed.as_millis());
    if r.unstable {
        println!("  ! 多轮枚举后仍有新进程出现，可能有进程在不断派生子进程");
    }
    print_failures(&r.failures);
}

fn print_thaw(r: &ThawReport) {
    if r.job_missing {
        println!("  ! 找不到环境的 Job（holder 可能已退出），已按冻结记录恢复");
    }
    if !r.had_record {
        println!("环境 `{}` 没有冻结记录，仍对其中所有进程执行了一次恢复", r.env);
    }
    println!("已恢复环境 `{}`：{} 个进程，用时 {} ms", r.env, r.resumed, r.elapsed.as_millis());
    println!("  注：内存会在程序被使用时按需换入，首次切回窗口可能有短暂卡顿");
    print_failures(&r.failures);
    if !r.ok() {
        println!("冻结记录已保留，可再次执行 `envfreeze thaw {}` 重试", r.env);
    }
}

fn print_batch(batch: &Batch) {
    println!("== {} ==", batch.title);
    if batch.steps.is_empty() {
        println!("无需操作");
    }
    for step in &batch.steps {
        match &step.result {
            Ok(Action::Froze(r)) => print_freeze(r),
            Ok(Action::Thawed(r)) => print_thaw(r),
            Ok(Action::Launched(r)) => print_launch(r),
            Ok(Action::AlreadyActive(env)) => println!("环境 `{env}` 已在运行"),
            Err(e) => eprintln!("环境 `{}` 出错：{e}", step.env),
        }
    }
}

fn print_failures(failures: &[Failure]) {
    if failures.is_empty() {
        return;
    }
    eprintln!("失败 {} 个：", failures.len());
    for f in failures {
        eprintln!("  PID {} {}：{}", f.pid, f.name, f.error);
    }
}

fn print_status(config: &Config) -> Result<()> {
    if config.environments.is_empty() {
        println!("配置中还没有环境");
        return Ok(());
    }
    println!("{:<20}  {:>5}  {:>11}  {:>11}  状态", "环境", "进程", "工作集", "私有提交");
    for s in status::all(config)? {
        let total = s.total();
        println!(
            "{:<20}  {:>5}  {:>11}  {:>11}  {}",
            s.name,
            s.processes.len(),
            fmt_bytes(total.working_set),
            fmt_bytes(total.private),
            s.state.label()
        );
    }
    Ok(())
}

fn print_status_detail(config: &Config, env: &str) -> Result<()> {
    config.env(env)?;
    let s = status::env_status(config, env)?;
    println!("环境 `{env}`：{}，{} 个进程", s.state.label(), s.processes.len());
    if s.processes.is_empty() {
        return Ok(());
    }
    println!("{:>7}  {:>11}  {:>11}  进程", "PID", "工作集", "私有提交");
    for p in &s.processes {
        let excluded = p.excluded_by.as_ref().map(|r| format!("  [白名单 {r}]")).unwrap_or_default();
        println!(
            "{:>7}  {:>11}  {:>11}  {}{excluded}",
            p.pid,
            fmt_bytes(p.memory.working_set),
            fmt_bytes(p.memory.private),
            p.name
        );
    }
    Ok(())
}
