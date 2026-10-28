# envfreeze

把一个"项目环境"里的整套开发程序（GoLand、Navicat、终端、Claude Code、codex……）当作一组：

- `freeze`：挂起整组进程（CPU 归零），清空它们的工作集，让物理内存降下来；
- `thaw`：恢复整组进程，内存在被用到时按需换入，窗口和状态保持原样。

不做进程快照，不杀进程，不碰虚拟机，只靠"挂起 + 清空工作集"，由系统把页面换出到内存压缩区和 pagefile。

## 用法

```bash
cargo build --release
```

把 [envfreeze.example.toml](envfreeze.example.toml) 复制到 `%APPDATA%\envfreeze\config.toml` 后按需修改（也可以用 `--config` 或环境变量 `ENVFREEZE_CONFIG` 指定路径）。

```text
envfreeze launch <环境名>   启动环境中的所有程序
envfreeze freeze <环境名>   冻结：挂起并换出内存，打印每个进程的内存释放量
envfreeze thaw   <环境名>   恢复
envfreeze status [环境名]   各环境状态；带环境名时列出进程明细
```

加 `-v` 输出调试日志。退出码：0 全部成功，1 部分失败（已汇总），2 出错。

## 工作原理

```text
envfreeze launch A ──启动──▶ envfreeze __hold A（隐藏，不在 Job 里）
                                │ 创建并持有命名 Job  Local\envfreeze_A
                                │ CreateProcess(挂起) → 加入 Job → 恢复
                                ▼
                     GoLand / Navicat / pwsh ……（及它们派生的所有子进程）
envfreeze freeze A ──按名字打开 Job──▶ 枚举 PID → NtSuspendProcess → EmptyWorkingSet
```

- **为什么需要 holder**：内核对象的名字只在至少有一个句柄打开时存在，进程对 Job 的引用不算句柄。如果 `launch` 退出后没有进程持有句柄，Job 的名字就会消失，`freeze` 也就找不到它了。holder 一直持有句柄，等 Job 中的进程全部退出后才自行退出，几乎不占资源。日志在 `%LOCALAPPDATA%\envfreeze\logs\<环境名>.log`。
- **Job 设置**：不设 `KILL_ON_JOB_CLOSE`，所以 holder 或本工具退出不会杀掉程序。设置了 `BREAKAWAY_OK`，只有显式请求脱离的进程才会离开 Job。这样在环境 A 的终端里执行 `launch B` 时，B 的 holder 能脱离 A。
- **冻结**：挂起过程中进程可能还在派生子进程，所以会反复枚举，直到不再出现新 PID。之后先写冻结记录（`%LOCALAPPDATA%\envfreeze\state\<环境名>.frozen`），再清空工作集。
- **防止重复冻结**：挂起是计数的，冻结两次就需要恢复两次。冻结记录存在时，`freeze` 会直接跳过。
- **不会留下半冻结状态**：单个进程失败时继续处理其余进程，最后汇总报错。`thaw` 有失败时会保留冻结记录，可以放心重试；对未挂起的进程执行恢复没有副作用。如果 holder 意外退出导致 Job 找不到了，`thaw` 会按记录里的 PID 和创建时间恢复。
- **白名单**：explorer.exe、系统进程、Defender 等安全软件以及本工具自身，永远不会被冻结。

## 环境内的程序必须由本工具启动

只有 `launch` 启动的程序（及其子进程）才属于环境。手动打开的程序不受管理。单实例程序要特别注意：

### GoLand：每个环境一个独立实例

GoLand 是单实例程序：再次运行 `goland64.exe <项目>` 时，它只会把项目交给已经在运行的 GoLand 进程，然后立即退出。`launch` 会检测到这种"启动后 3 秒内退出"的情况并给出警告，但这样项目还是落在了环境外的进程里。

解决办法是给每个环境指定独立的配置目录和系统目录：

1. 复制 `D:\idea\GoLand 2026.1\bin\idea.properties` 到例如 `%APPDATA%\envfreeze\goland\proj-a\idea.properties`，并在末尾追加：

   ```properties
   idea.config.path=${user.home}/.envfreeze-goland/proj-a/config
   idea.system.path=${user.home}/.envfreeze-goland/proj-a/system
   idea.plugins.path=${idea.config.path}/plugins
   idea.log.path=${idea.system.path}/log
   ```

2. 在配置中给这个 GoLand 程序设置 `env = { GOLAND_PROPERTIES = '...该文件路径...' }`（见示例配置）。
3. 想沿用现有设置，可以把 `%APPDATA%\JetBrains\GoLand2026.1` 复制到新的 config 目录（可能需要重新登录许可证），或者使用 Settings Sync。

同一个环境里的多个仓库放在这一个实例里，以多个窗口打开（在配置的 `args` 中列出多个项目路径），它们共用一个 JVM。

### Navicat

Navicat 也是单实例程序，无法拆分到多个环境，只能归属其中一个环境。如果它已经被手动打开，请先关掉，再用 `launch` 启动。

### 终端

不要在配置中使用 `wt.exe`。Windows Terminal 是打包应用而且是单实例，从它启动的进程不会进入环境。请直接启动 `pwsh.exe` / `cmd.exe`，每个程序都会获得独立的控制台窗口。Claude Code 和 codex 在这些终端里运行，会自动跟着进入环境。

## 已知限制

- **网络连接**：冻结期间网络连接可能超时断开，例如数据库连接、Claude Code / codex 正在进行的流式任务。请在程序空闲时冻结。
- **恢复卡顿**：恢复本身是瞬间完成的，但内存要等程序被用到时才从压缩区或 pagefile 换入，所以首次切回窗口时会有卡顿。
- **pagefile 大小**：冻结不会减少提交内存（commit），pagefile 必须足够大，能容纳所有冻结环境的私有提交。
- **"未响应"**：点击被冻结的窗口后，标题栏会显示"未响应"。如果 Windows 弹窗问是否关闭程序，**不要点"关闭程序"**，否则进程会被直接结束。
- **可用内存的统计**：清空工作集后，页面会先进入修改列表或内存压缩区，所以 `freeze` 打印的"系统可用物理内存"可能要过几秒才会继续上升。工作集一栏的下降量是即时、准确的。如果要做完整对比，请同时观察任务管理器中的"使用中"和"已压缩"。
- **权限**：不要以管理员身份运行本工具，否则它启动的所有程序也都会以管理员身份运行。只有当环境里的进程本身是提权运行时，才需要在管理员终端中执行 freeze/thaw，权限不足时工具会给出明确提示。
- **托管终端**：如果在会因关闭而结束子进程的终端里执行 `launch`，而 holder 又无法脱离该终端所在的 Job，工具会给出警告。此时关闭该终端可能会连带结束整个环境。
