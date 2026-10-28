//! 内存统计和格式化。

use std::iter::Sum;
use std::mem::size_of;
use std::ops::Add;

use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// 单个进程（或一组进程之和）的内存。
#[derive(Debug, Clone, Copy, Default)]
pub struct Memory {
    /// 工作集：当前驻留在物理内存中的部分。
    pub working_set: u64,
    /// 私有提交：进程独占的已提交内存，冻结不会让它下降。
    pub private: u64,
}

impl Add for Memory {
    type Output = Memory;

    fn add(self, other: Memory) -> Memory {
        Memory { working_set: self.working_set + other.working_set, private: self.private + other.private }
    }
}

impl Sum for Memory {
    fn sum<I: Iterator<Item = Memory>>(iter: I) -> Memory {
        iter.fold(Memory::default(), Add::add)
    }
}

/// 系统可用物理内存（字节）。
pub fn available_physical() -> Option<u64> {
    let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.ullAvailPhys)
}

pub fn fmt_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let mib = bytes as f64 / MIB;
    if mib >= 1024.0 { format!("{:.2} GiB", mib / 1024.0) } else { format!("{mib:.1} MiB") }
}

/// 带符号的变化量，例如 `-1.20 GiB`。
pub fn fmt_delta(before: u64, after: u64) -> String {
    if after >= before { format!("+{}", fmt_bytes(after - before)) } else { format!("-{}", fmt_bytes(before - after)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(fmt_bytes(512 * 1024 * 1024), "512.0 MiB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
        assert_eq!(fmt_delta(2 * 1024 * 1024, 1024 * 1024), "-1.0 MiB");
    }
}
