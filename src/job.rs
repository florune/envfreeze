//! 每个环境对应一个命名 Job Object：`Local\envfreeze_<环境名>`。
//!
//! 内核对象的名字只在至少有一个句柄打开时存在（进程对 Job 的引用不算句柄），
//! 所以由 holder 进程创建并一直持有句柄；CLI 每次按名字打开即可，自身无状态。

use std::ffi::c_void;
use std::mem::{offset_of, size_of};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, GetLastError, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
    SetInformationJobObject,
};
use windows::core::PCWSTR;

use crate::win::{OwnedHandle, is_win32, wide};

const JOB_OBJECT_QUERY: u32 = 0x0004;

/// 完成端口消息：Job 中已没有存活进程。
pub const MSG_ACTIVE_PROCESS_ZERO: u32 = 4;

pub fn object_name(env: &str) -> String {
    format!("Local\\envfreeze_{env}")
}

pub struct Job {
    handle: OwnedHandle,
}

impl Job {
    /// 创建环境的 Job。同名 Job 已存在说明环境已在运行，返回错误。
    ///
    /// 不设置 KILL_ON_JOB_CLOSE：holder 退出不能杀掉环境里的程序。
    /// 设置 BREAKAWAY_OK：只有显式要求脱离的进程才会离开 Job，
    /// 这样在环境 A 的终端里 launch 环境 B 时，B 的 holder 能脱离 A。
    pub fn create(env: &str) -> Result<Self> {
        let name = wide(object_name(env));
        let handle = unsafe { CreateJobObjectW(None, PCWSTR(name.as_ptr())) }.context("CreateJobObjectW 失败")?;
        let handle = OwnedHandle::new(handle);
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            bail!("环境 `{env}` 已在运行（Job {} 已存在）", object_name(env));
        }

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        unsafe {
            SetInformationJobObject(
                handle.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .context("设置 Job 限制失败")?;

        Ok(Self { handle })
    }

    /// 按名字打开环境的 Job；环境未运行（没有 holder 持有句柄）时返回 None。
    pub fn open(env: &str) -> Result<Option<Self>> {
        let name = wide(object_name(env));
        match unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, false, PCWSTR(name.as_ptr())) } {
            Ok(handle) => Ok(Some(Self { handle: OwnedHandle::new(handle) })),
            Err(e) if is_win32(&e, ERROR_FILE_NOT_FOUND) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("打开 Job {} 失败", object_name(env))),
        }
    }

    pub fn assign(&self, process: HANDLE) -> Result<()> {
        unsafe { AssignProcessToJobObject(self.handle.raw(), process) }.context("AssignProcessToJobObject 失败")
    }

    /// 关联完成端口，用来接收"Job 已空"等通知。
    pub fn attach_port(&self, port: HANDLE) -> Result<()> {
        let info =
            JOBOBJECT_ASSOCIATE_COMPLETION_PORT { CompletionKey: std::ptr::null_mut::<c_void>(), CompletionPort: port };
        unsafe {
            SetInformationJobObject(
                self.handle.raw(),
                JobObjectAssociateCompletionPortInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        }
        .context("关联 Job 完成端口失败")
    }

    /// Job 中当前所有进程（包括嵌套子 Job 中的）的 PID。
    pub fn pids(&self) -> Result<Vec<u32>> {
        // 缓冲区按 usize 分配：头部两个 u32 之后紧跟 PID 数组。
        const HEADER: usize = offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList) / size_of::<usize>();
        let mut capacity = 64;
        loop {
            let mut buf = vec![0usize; HEADER + capacity];
            let result = unsafe {
                QueryInformationJobObject(
                    Some(self.handle.raw()),
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * size_of::<usize>()) as u32,
                    None,
                )
            };
            let list = unsafe { &*buf.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            match result {
                Ok(()) => {
                    let count = list.NumberOfProcessIdsInList as usize;
                    return Ok(buf[HEADER..HEADER + count].iter().map(|&pid| pid as u32).collect());
                }
                Err(e) if is_win32(&e, ERROR_MORE_DATA) => {
                    capacity = (list.NumberOfAssignedProcesses as usize).max(capacity * 2) + 16;
                }
                Err(e) => return Err(e).context("查询 Job 进程列表失败"),
            }
        }
    }
}
