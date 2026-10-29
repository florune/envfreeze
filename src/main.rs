//! envfreeze 命令行。

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use envfreeze::config::{self, Config};
use envfreeze::freeze::{Failure, FreezeReport, ThawReport};
use envfreeze::launch::{LaunchReport, Level};
use envfreeze::memory::{fmt_bytes, fmt_delta};
use envfreeze::stop::{self, StopReport};
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
    /// 关闭环境：冻结中先恢复，再关闭所有窗口，等程序自行退出
    Stop {
        env: String,
        /// 等待程序自行退出的秒数
        #[arg(long, default_value_t = stop::DEFAULT_TIMEOUT.as_secs())]
        timeout: u64,
        /// 超时后强制结束仍未退出的进程（未保存的内容会丢失）
        #[arg(long)]
        force: bool,
    },
    /// 立即强制结束环境中的所有进程，不等程序保存（未保存的内容会丢失）
    Kill { env: String },
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
        Command::Stop { env, timeout, force } => {
            config::validate_env_name(env)?;
            let report = stop::stop(env, std::time::Duration::from_secs(*timeout), *force)?;
            print_stop(&report);
            Ok(report.ok())
        }
        Command::Kill { env } => {
            config::validate_env_name(env)?;
            let report = stop::kill(env)?;
            print_stop(&report);
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
    if r.taskbar_windows() > 0 {
        let titles: Vec<&str> = r.hidden.iter().filter(|w| w.in_taskbar).map(|w| w.title.as_str()).collect();
        println!("隐藏 {} 个窗口（恢复时原样显示）：{}", titles.len(), titles.join("、"));
    }
    if r.stuck_windows > 0 {
        println!("  ! {} 个窗口没能隐藏：程序当时未响应，冻结后会显示「未响应」", r.stuck_windows);
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
    println!(
        "已恢复环境 `{}`：{} 个进程，显示 {} 个窗口，用时 {} ms",
        r.env,
        r.resumed,
        r.shown_windows,
        r.elapsed.as_millis()
    );
    println!("  注：内存会在程序被使用时按需换入，首次切回窗口可能有短暂卡顿");
    print_failures(&r.failures);
    if !r.ok() {
        println!("冻结记录已保留，可再次执行 `envfreeze thaw {}` 重试", r.env);
    }
}

fn print_stop(r: &StopReport) {
    if let Some(thawed) = &r.thawed {
        print_thaw(thawed);
    }
    if r.closed_windows > 0 {
        println!("已请求关闭 {} 个窗口", r.closed_windows);
    }
    if r.killed > 0 {
        println!("强制结束 {} 个进程", r.killed);
    }
    if r.remaining.is_empty() {
        println!("环境 `{}` 已关闭，用时 {} ms", r.env, r.elapsed.as_millis());
    } else {
        println!("环境 `{}` 还有 {} 个进程没有退出：", r.env, r.remaining.len());
        for l in &r.remaining {
            println!("  PID {} {}", l.pid, l.name);
        }
        println!("  程序可能正在等你确认（例如保存或退出提示），处理后再执行一次 stop；");
        println!("  或者执行 `envfreeze kill {}` 强制结束（未保存的内容会丢失）", r.env);
    }
    print_failures(&r.failures);
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
    if !s.hidden_windows.is_empty() {
        println!("隐藏的窗口：");
        for title in &s.hidden_windows {
            println!("  {title}");
        }
    }
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
