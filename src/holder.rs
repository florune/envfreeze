//! holder：每个运行中的环境对应一个隐藏的后台进程（`envfreeze __hold <环境名>`）。
//!
//! 它创建环境的命名 Job 并一直持有句柄（否则 Job 名字会消失），
//! 负责启动配置中的程序，然后等到 Job 中没有存活进程时自行退出。
//! holder 本身不在 Job 里，冻结环境时不会被挂起。
//!
//! 启动结果通过 stdout 逐行回报给 `launch`：`OK|WARN|ERR\t消息`，最后一行 `DONE`。

use std::collections::BTreeMap;
use std::io::Write;
use std::iter::repeat_n;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};
use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED};
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, INFINITE, PROCESS_INFORMATION,
    ResumeThread, STARTUPINFOW, TerminateProcess,
};
use windows::Win32::UI::Shell::{SHGFI_EXETYPE, SHGetFileInfoW};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::config::{self, Config, Program};
use crate::job::{self, Job};
use crate::process::Process;
use crate::state;
use crate::win::{OwnedHandle, wide};

/// 启动后这么久内退出的程序，多半是把请求转交给了已在运行的单实例。
const SINGLE_INSTANCE_GRACE: Duration = Duration::from_secs(3);

pub const DONE: &str = "DONE";

/// 向 launch 回报一行结果，同时写日志。launch 已退出时写失败可以忽略。
fn report(kind: &str, msg: &str) {
    match kind {
        "ERR" => error!("{msg}"),
        "WARN" => warn!("{msg}"),
        _ => info!("{msg}"),
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{kind}\t{}", msg.replace(['\r', '\n'], " "));
    let _ = out.flush();
}

/// holder 进程入口（命令行和托盘程序的 `__hold` 子命令）。
/// holder 没有控制台，日志追加到 `%LOCALAPPDATA%\envfreeze\logs\<环境名>.log`。
pub fn main(config_path: &Path, env: &str) -> Result<()> {
    config::validate_env_name(env)?;
    let path = crate::paths::holder_log(env)?;
    std::fs::create_dir_all(path.parent().expect("日志文件必有父目录"))?;
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("envfreeze=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    run(config_path, env)
}

fn run(config_path: &Path, env: &str) -> Result<()> {
    info!(env, config = %config_path.display(), "holder 启动");
    let result = launch_all(config_path, env);
    if let Err(e) = &result {
        report("ERR", &format!("{e:#}"));
    }
    report(DONE, "");

    // job 必须活到最后：它的句柄维持着 Job 的名字。
    let (_job, port) = match result {
        Ok(Some(running)) => running,
        Ok(None) => {
            info!("没有程序成功加入环境，holder 退出");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    wait_until_empty(&port)?;
    info!("环境中的进程已全部退出，holder 退出");
    // 环境已经结束，残留的冻结记录没有意义了。
    state::clear(env)
}

/// 创建 Job、启动全部程序。至少一个程序成功加入 Job 时返回 Job 和完成端口。
fn launch_all(config_path: &Path, env: &str) -> Result<Option<(Job, OwnedHandle)>> {
    let config = Config::load(config_path)?;
    let environment = config.env(env)?;
    let job = Job::create(env)?;
    let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) }.context("创建完成端口失败")?;
    let port = OwnedHandle::new(port);
    job.attach_port(port.raw())?;
    info!(job = job::object_name(env), "已创建 Job");

    let mut launched = Vec::new();
    for program in &environment.programs {
        let name = program.display_name();
        match spawn_in_job(&job, program) {
            Ok(process) => {
                report("OK", &format!("{name} 已启动 (PID {})", process.pid));
                launched.push((name, process));
            }
            Err(e) => report("ERR", &format!("{name} 启动失败：{e:#}")),
        }
    }
    if launched.is_empty() {
        return Ok(None);
    }

    thread::sleep(SINGLE_INSTANCE_GRACE);
    for (name, process) in &launched {
        if let Some(code) = process.exit_code() {
            report(
                "WARN",
                &format!(
                    "{name} (PID {}) 启动后很快退出（退出码 {code}）。如果它是单实例程序（Navicat、未隔离配置的 GoLand 等），\
                     请求已转交给环境外已在运行的实例，那个实例不受本环境管理",
                    process.pid
                ),
            );
        }
    }
    Ok(Some((job, port)))
}

/// 以挂起状态创建进程 → 加入 Job → 恢复主线程，保证它派生的任何子进程都在 Job 里。
fn spawn_in_job(job: &Job, program: &Program) -> Result<Process> {
    let path = config::expand(&program.path);
    let args: Vec<String> = program.args.iter().map(|a| config::expand(a)).collect();
    // 控制台程序经由 conhost 启动时，原来的程序和参数整体作为 conhost 的参数。
    let command_line = match console_host(&path) {
        Some(conhost) => build_command_line(&conhost, &[std::slice::from_ref(&path), &args].concat()),
        None => build_command_line(&path, &args),
    };
    // 没配工作目录时用用户主目录，而不是继承 holder 的（那是执行 launch 时所在的任意目录，
    // 有些程序会往工作目录里写文件）。
    let cwd = match program.cwd.as_deref() {
        Some(c) => Some(config::expand_path(c)),
        None => dirs::home_dir(),
    }
    .map(wide);
    let env_block = (!program.env.is_empty()).then(|| build_env_block(&program.env));
    debug!(command_line, cwd = program.cwd.as_deref(), "CreateProcessW");
    let mut command_line = wide(command_line);

    let startup = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            // 控制台程序（pwsh、cmd 等）各自获得新的控制台窗口；对 GUI 程序无影响。
            CREATE_SUSPENDED | CREATE_NEW_CONSOLE | CREATE_UNICODE_ENVIRONMENT,
            env_block.as_ref().map(|b| b.as_ptr().cast()),
            cwd.as_ref().map_or(PCWSTR::null(), |c| PCWSTR(c.as_ptr())),
            &startup,
            &mut info,
        )
    }
    .with_context(|| format!("CreateProcessW({path}) 失败"))?;

    let process = Process::from_handle(info.dwProcessId, OwnedHandle::new(info.hProcess));
    let thread = OwnedHandle::new(info.hThread);
    if let Err(e) = job.assign(process.handle()) {
        let _ = unsafe { TerminateProcess(process.handle(), 1) };
        return Err(e);
    }
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        let err = windows::core::Error::from_thread();
        let _ = unsafe { TerminateProcess(process.handle(), 1) };
        return Err(err).context("ResumeThread 失败");
    }
    Ok(process)
}

/// 控制台程序（pwsh、cmd 等）改由 conhost.exe 承载，返回 conhost.exe 的路径。
///
/// Windows 11 默认把新控制台转交给 Windows Terminal：窗口属于环境外的 WindowsTerminal.exe，
/// 所有环境的终端混在同一个进程里，冻结时既隐藏不了，也分不开。直接启动 conhost.exe 不会被转交，
/// 每个终端都有自己的窗口，并且窗口归属于环境里的进程。
fn console_host(path: &str) -> Option<String> {
    let resolved = resolve_exe(path)?;
    if resolved.file_name().is_some_and(|n| n.eq_ignore_ascii_case("conhost.exe")) || !is_console_app(&resolved) {
        return None;
    }
    let conhost = Path::new(&std::env::var_os("SystemRoot")?).join(r"System32\conhost.exe");
    conhost.is_file().then(|| conhost.to_string_lossy().into_owned())
}

/// 和 CreateProcess 一样，不带目录的程序名在 PATH 里查找。
fn resolve_exe(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_owned());
    }
    let names = [p.to_owned(), p.with_extension("exe")];
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|c| c.is_file())
}

fn is_console_app(path: &Path) -> bool {
    let info = unsafe {
        SHGetFileInfoW(&HSTRING::from(path.as_os_str()), FILE_FLAGS_AND_ATTRIBUTES(0), None, 0, SHGFI_EXETYPE)
    };
    // 低字为 "PE"、高字（子系统版本）为 0 表示控制台程序。
    info & 0xFFFF == 0x4550 && info >> 16 == 0
}

fn wait_until_empty(port: &OwnedHandle) -> Result<()> {
    loop {
        let mut message = 0u32;
        let mut key = 0usize;
        let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
        unsafe { GetQueuedCompletionStatus(port.raw(), &mut message, &mut key, &mut overlapped, INFINITE) }
            .context("等待 Job 通知失败")?;
        // 对 Job 通知而言，lpOverlapped 里放的是相关进程的 PID。
        debug!(message, pid = overlapped as usize, "Job 通知");
        if message == job::MSG_ACTIVE_PROCESS_ZERO {
            return Ok(());
        }
    }
}

/// 按 MSVC CRT 的规则拼接命令行，保证目标程序解析出的 argv 与配置一致。
fn build_command_line(program: &str, args: &[String]) -> String {
    let mut line = String::new();
    // 程序路径不可能含双引号，直接整体加引号即可。
    line.push('"');
    line.push_str(program);
    line.push('"');
    for arg in args {
        line.push(' ');
        quote_arg(arg, &mut line);
    }
    line
}

fn quote_arg(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0B', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// 继承当前环境变量并覆盖配置中的项，生成按名称排序（不区分大小写）的 UTF-16 环境块。
fn build_env_block(overrides: &BTreeMap<String, String>) -> Vec<u16> {
    let mut vars: BTreeMap<String, (String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .map(|(k, v)| (k.to_uppercase(), (k, v)))
        .collect();
    for (key, value) in overrides {
        vars.insert(key.to_uppercase(), (key.clone(), config::expand(value)));
    }
    let mut block: Vec<u16> = Vec::new();
    for (key, value) in vars.values() {
        block.extend(format!("{key}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(arg: &str) -> String {
        let mut out = String::new();
        quote_arg(arg, &mut out);
        out
    }

    #[test]
    fn quotes_args_like_msvcrt() {
        assert_eq!(quoted("plain"), "plain");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted(r"D:\my repo"), r#""D:\my repo""#);
        assert_eq!(quoted(r"D:\my repo\"), r#""D:\my repo\\""#);
        assert_eq!(quoted(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quoted(r"a\b"), r"a\b");
    }

    #[test]
    fn builds_command_line() {
        let line = build_command_line(r"C:\Program Files\x.exe", &["-a".into(), "b c".into()]);
        assert_eq!(line, r#""C:\Program Files\x.exe" -a "b c""#);
    }

    #[test]
    fn env_block_overrides_case_insensitively() {
        let overrides = BTreeMap::from([("path".to_string(), "X".to_string())]);
        let block = String::from_utf16(&build_env_block(&overrides)).unwrap();
        let entries: Vec<&str> = block.split('\0').filter(|s| !s.is_empty()).collect();
        assert!(entries.contains(&"path=X"));
        assert!(!entries.iter().any(|e| e.to_uppercase().starts_with("PATH=") && *e != "path=X"));
        assert!(block.ends_with("\0\0"));
    }
}
