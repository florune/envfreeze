//! 按 PID 打开进程并读取名称、创建时间、内存等信息。

use std::mem::size_of;
use std::path::Path;

use windows::Win32::Foundation::{FILETIME, HANDLE, STILL_ACTIVE};
use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

use crate::memory::Memory;
use crate::win::OwnedHandle;

pub struct Process {
    pub pid: u32,
    handle: OwnedHandle,
}

impl Process {
    pub fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> windows::core::Result<Self> {
        let handle = unsafe { OpenProcess(access, false, pid) }?;
        Ok(Self { pid, handle: OwnedHandle::new(handle) })
    }

    pub fn from_handle(pid: u32, handle: OwnedHandle) -> Self {
        Self { pid, handle }
    }

    pub fn handle(&self) -> HANDLE {
        self.handle.raw()
    }

    /// 映像文件名，例如 `goland64.exe`。需要 PROCESS_QUERY_LIMITED_INFORMATION。
    pub fn image_name(&self) -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        unsafe { QueryFullProcessImageNameW(self.handle(), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }
            .ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&full).file_name().map(|n| n.to_string_lossy().into_owned())
    }

    /// 创建时间（FILETIME 的 100ns 计数），用来识别 PID 是否被复用。
    pub fn creation_time(&self) -> Option<u64> {
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetProcessTimes(self.handle(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
        Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    pub fn memory(&self) -> Option<Memory> {
        let mut counters =
            PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        unsafe {
            GetProcessMemoryInfo(self.handle(), (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(), counters.cb)
        }
        .ok()?;
        Some(Memory { working_set: counters.WorkingSetSize as u64, private: counters.PrivateUsage as u64 })
    }

    /// 把工作集里的页面全部移出物理内存（进入备用/修改列表、压缩存储或 pagefile）。
    /// 需要 PROCESS_SET_QUOTA 和 PROCESS_QUERY_LIMITED_INFORMATION。
    pub fn empty_working_set(&self) -> windows::core::Result<()> {
        unsafe { EmptyWorkingSet(self.handle()) }
    }

    /// 进程已退出时返回退出码，仍在运行时返回 None。
    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.handle(), &mut code) }.ok()?;
        (code != STILL_ACTIVE.0 as u32).then_some(code)
    }
}
