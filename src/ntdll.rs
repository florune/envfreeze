//! ntdll 中未公开但稳定多年的进程挂起/恢复接口。
//!
//! NtSuspendProcess 会把进程内每个线程的挂起计数加一，NtResumeProcess 减一，
//! 所以同一进程冻结两次就需要恢复两次；上层通过冻结记录保证不重复冻结。

use std::ffi::c_void;

use anyhow::{Result, bail};
use windows::Win32::Foundation::HANDLE;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtSuspendProcess(process: *mut c_void) -> i32;
    fn NtResumeProcess(process: *mut c_void) -> i32;
}

pub fn suspend(process: HANDLE) -> Result<()> {
    check("NtSuspendProcess", unsafe { NtSuspendProcess(process.0) })
}

pub fn resume(process: HANDLE) -> Result<()> {
    check("NtResumeProcess", unsafe { NtResumeProcess(process.0) })
}

fn check(func: &str, status: i32) -> Result<()> {
    if status < 0 {
        bail!("{func} 失败，NTSTATUS 0x{:08X}", status as u32);
    }
    Ok(())
}
