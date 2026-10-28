//! 白名单：这些进程无论如何都不冻结。
//!
//! 正常情况下 Job 里只有本工具启动的程序，这里是兜底，
//! 防止 shell 打开文件夹之类的操作把系统进程带进环境。

/// 小写的映像文件名。
const PROTECTED: &[&str] = &[
    "system",
    "registry",
    "memory compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "svchost.exe",
    "fontdrvhost.exe",
    "dwm.exe",
    "explorer.exe",
    "sihost.exe",
    "ctfmon.exe",
    "taskmgr.exe",
    "msmpeng.exe",
    "nissrv.exe",
    "mpdefendercoreservice.exe",
    "securityhealthservice.exe",
    "securityhealthsystray.exe",
    "smartscreen.exe",
    "envfreeze.exe",
];

/// 返回不能冻结的原因；可以冻结时返回 None。
pub fn protected_reason(pid: u32, image_name: Option<&str>) -> Option<&'static str> {
    if pid <= 4 {
        return Some("系统进程");
    }
    if pid == std::process::id() {
        return Some("本工具自身");
    }
    let name = image_name?.to_ascii_lowercase();
    PROTECTED.contains(&name.as_str()).then_some("白名单进程")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_system_and_self() {
        assert!(protected_reason(4, Some("System")).is_some());
        assert!(protected_reason(1234, Some("Explorer.EXE")).is_some());
        assert!(protected_reason(std::process::id(), Some("whatever.exe")).is_some());
        assert!(protected_reason(1234, Some("goland64.exe")).is_none());
        assert!(protected_reason(1234, None).is_none());
    }
}
