//! envfreeze 核心：按"项目环境"成套冻结和恢复 Windows 上的开发程序。
//!
//! 命令行（`envfreeze`）和托盘程序（`envfreeze-tray`）共用这里的全部逻辑。
//! 所有操作都是无状态的：环境状态保存在命名 Job Object 和冻结记录文件中，
//! 因此命令行、托盘、面板可以同时使用，互不干扰。

pub mod config;
pub mod config_edit;
pub mod exclude;
pub mod freeze;
mod guard;
pub mod holder;
pub mod jetbrains;
mod job;
pub mod launch;
pub mod memory;
mod ntdll;
pub mod paths;
mod process;
mod state;
pub mod status;
pub mod switch;
mod win;
