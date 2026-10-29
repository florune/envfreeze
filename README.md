# envfreeze

[![Release](https://img.shields.io/github/v/release/florune/envfreeze)](https://github.com/florune/envfreeze/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011%20x64-0078d4)

**像切换虚拟桌面一样切换项目环境，并把暂时不用的环境从内存里腾出来。**

同时开着好几个项目时，每个项目都有一整套程序：IDE、数据库客户端、几个终端、终端里跑着的 Claude Code / codex……内存很快就不够用，任务栏也挤满了分不清是哪个项目的窗口。

envfreeze 把每个项目的这套程序当作一个**环境**来管理：

- **冻结**：环境里的窗口从任务栏和 Alt+Tab 里整体消失，进程被挂起（CPU 占用归零），内存被换出，物理内存随之释放。
- **恢复**：进程继续运行，窗口回到原来的位置，上次在用的窗口自动回到前台。程序状态原样保留，不需要重新打开项目。
- **切换**：一个快捷键完成"冻结当前环境 → 恢复或启动目标环境"。

不杀进程、不做快照、不用虚拟机，只用 Windows 自带的进程挂起和工作集换出机制。

> 实测：一个包含 GoLand 的环境冻结后，释放了 2.7 GiB 物理内存（GoLand 本身及其派生的 git、AI 插件等 19 个进程）。

## 目录

- [功能](#功能)
- [安装](#安装)
- [快速开始](#快速开始)
- [托盘与面板](#托盘与面板)
- [配置](#配置)
- [命令行](#命令行)
- [关闭环境](#关闭环境)
- [单实例程序](#单实例程序)
- [工作原理](#工作原理)
- [已知限制](#已知限制)
- [从源码构建](#从源码构建)
- [许可证](#许可证)

## 功能

- **按环境成组管理**：环境里的程序由 envfreeze 启动，它们派生的所有子进程（git、语言服务器、终端里的命令等）都自动归入环境。
- **窗口随环境隐藏**：冻结时隐藏环境的全部窗口，不会留下"未响应"的白窗；恢复时保持原来的位置、大小和最大化/最小化状态。
- **一键切换**：每个环境可以绑定一个全局快捷键，完成后弹出系统通知，告诉你释放了多少内存。
- **托盘常驻、占用极低**：托盘只占约 14 MiB 工作集；图形面板按需启动，关闭即释放。
- **可视化配置**：在面板里从"正在运行 / 开始菜单 / 终端"中挑选程序，不需要手写配置文件。
- **JetBrains IDE 自动隔离**：GoLand、IDEA 等 IDE 自动为每个环境生成独立实例，分别冻结、互不干扰。
- **白名单**：需要保持连接的程序（例如 Navicat）可以设为冻结时跳过。
- **安全关闭**：像点 X 一样关闭整个环境，让程序自己保存退出；卡死时再由你决定是否强制结束。
- **命令行与托盘共用状态**：所有状态都保存在系统对象和状态文件里，两者可以混用。

## 安装

系统要求：Windows 10 或 Windows 11，x64。

### 方式一：下载编译好的程序（推荐）

1. 从 [Releases](https://github.com/florune/envfreeze/releases/latest) 下载 `envfreeze-<版本>-x86_64-pc-windows-msvc.zip`。
2. 解压到一个固定目录，例如 `%LOCALAPPDATA%\Programs\envfreeze`。
3. 双击 `envfreeze-tray.exe` 启动托盘。需要命令行的话，把这个目录加入 `PATH`。

压缩包里有：

| 文件 | 说明 |
|---|---|
| `envfreeze-tray.exe` | 托盘程序和图形面板，日常使用只需要它 |
| `envfreeze.exe` | 命令行工具 |
| `envfreeze.example.toml` | 带注释的配置示例 |

两个 exe 都静态链接了运行库，不需要额外安装 VC++ 运行库。程序没有代码签名，首次运行时 Windows SmartScreen 可能会提示"已保护你的电脑"，点「更多信息 → 仍要运行」即可。可以用 Release 页面里的 `SHA256SUMS.txt` 校验下载的文件。

### 方式二：用 Cargo 安装

已经装了 [Rust](https://rustup.rs/) 的话：

```powershell
cargo install --locked --git https://github.com/florune/envfreeze --tag v0.2.0
```

`envfreeze.exe` 和 `envfreeze-tray.exe` 会安装到 `%USERPROFILE%\.cargo\bin`。

### 升级

先关闭所有运行中的环境，并退出托盘，再替换 exe 或重新执行 `cargo install`。环境运行期间，后台的 holder 进程会占用 exe，文件替换会失败。

## 快速开始

1. 启动 `envfreeze-tray.exe`，右下角会出现一个圆形图标。左键单击打开面板。
2. 在面板「环境」页顶部输入环境名（例如 `proj-a`），点「创建」。
3. 点「＋ 添加程序」，选择这个项目要用的程序，填好参数（例如项目目录），点「添加到环境」。
4. 在「快捷键」页给环境录一个切换快捷键，例如 `Ctrl+Alt+1`。
5. 按快捷键：envfreeze 会冻结其他运行中的环境，然后启动这个环境。之后再按，就在环境之间来回切换。

配置文件默认保存在 `%APPDATA%\envfreeze\config.toml`，第一次打开面板时会自动生成。

## 托盘与面板

托盘图标的颜色表示整体状态：冰蓝表示有环境被冻结，绿色表示只有运行中的环境，灰色表示没有环境在运行。

- **全局快捷键**：
  - 每个环境的「切换」快捷键；
  - 「冻结全部」；
  - 「打开面板」。
- **右键菜单**：
  - 每个环境都可以单独切换、启动、冻结、恢复、关闭；
  - 冻结的环境会显示隐藏了几个窗口，子菜单里列出这些窗口的标题；
  - 菜单里还有冻结全部、打开面板、编辑配置文件、打开日志目录、开机自启和退出。
- **左键单击**：打开面板。
- **面板**（按 Esc 关闭）：

  | 页面 | 内容 |
  |---|---|
  | 环境 | 新建、删除环境，添加、删除程序；启动、冻结、恢复、关闭、强制结束环境；查看状态、内存、隐藏的窗口和进程明细，在进程明细里勾选「冻结时跳过」 |
  | 快捷键 | 点「录制」后按下组合键 |
  | 白名单 | 编辑全局白名单和各环境的白名单 |
  | 设置与日志 | 开机自启，以及本次的操作记录 |

面板的修改会直接写回配置文件，只改动对应的键，保留注释。托盘每 2 秒检查一次配置文件，手动编辑同样会立即生效。

托盘常驻约 14 MiB 工作集（私有内存约 2.4 MiB）。面板是独立进程，打开期间约 190 MiB（主要是图形驱动的开销），关闭后全部释放。

## 配置

完整的带注释示例见 [envfreeze.example.toml](envfreeze.example.toml)。两个程序都支持用 `--config <路径>` 或环境变量 `ENVFREEZE_CONFIG` 指定配置文件。

```toml
exclude = []                       # 全局白名单

[hotkeys]
panel = "Ctrl+Alt+Space"           # 打开面板
freeze_all = "Ctrl+Alt+0"          # 冻结所有运行中的环境

[environments.proj-a]
hotkey = "Ctrl+Alt+1"              # 切换到此环境
exclude = ["navicat"]              # 本环境的白名单

[[environments.proj-a.programs]]
name = "GoLand"
path = 'D:\idea\GoLand 2026.1\bin\goland64.exe'
args = ['D:\code\proj-a\api', 'D:\code\proj-a\web']
env = { GOLAND_PROPERTIES = '%APPDATA%\envfreeze\goland\proj-a\idea.properties' }

[[environments.proj-a.programs]]
path = 'C:\Program Files\PowerShell\7\pwsh.exe'
cwd = 'D:\code\proj-a'
```

每个程序可以配置：

| 键 | 说明 |
|---|---|
| `name` | 显示名，可选，默认取可执行文件名 |
| `path` | 可执行文件路径，请指向 `.exe` |
| `args` | 参数列表，可选 |
| `cwd` | 工作目录，可选，默认为用户主目录 |
| `env` | 额外的环境变量，可选 |

`path`、`args`、`cwd`、`env` 的值都支持 `%VAR%` 展开。

### 白名单

冻结时希望某些程序继续运行（例如需要保持数据库连接的 Navicat），可以把它加入白名单：

```toml
exclude = ["navicat"]              # 全局：所有环境都跳过

[environments.proj-a]
exclude = ["node*", "pwsh.exe"]    # 只对 proj-a 生效
```

- 按进程映像名匹配，不区分大小写，支持 `*` 和 `?` 通配符；不写扩展名也能匹配 `.exe`。
- 被跳过的进程不会被挂起，也不会被换出内存，但它的窗口照样会隐藏，恢复环境时再显示。
- 白名单在下一次冻结时生效。
- 系统进程、`explorer.exe`、安全软件和 envfreeze 自身由内置规则保护，无论配置如何都不会被冻结。

## 命令行

```text
envfreeze launch <环境>        启动环境中的所有程序
envfreeze freeze <环境>        冻结：隐藏窗口、挂起进程、换出内存
envfreeze thaw   <环境>        恢复
envfreeze switch <环境>        切换：冻结其他运行中的环境，再启动或恢复目标环境
envfreeze freeze-all           冻结所有运行中的环境
envfreeze stop   <环境>        关闭：冻结中先恢复，再关闭所有窗口，等程序自行退出
                 [--timeout 秒]   等待时长，默认 15 秒
                 [--force]        超时后强制结束剩下的进程
envfreeze kill   <环境>        立即强制结束环境中的所有进程（未保存的内容会丢失）
envfreeze status [环境]        各环境状态；指定环境时列出进程明细和隐藏的窗口
```

全局选项：

| 选项 | 说明 |
|---|---|
| `--config <路径>` | 指定配置文件 |
| `-v` | 输出调试日志 |

退出码：`0` 全部成功，`1` 部分失败（已汇总），`2` 出错。

## 关闭环境

「关闭环境」（命令行 `stop`）的效果和挨个点窗口右上角的 X 一样：

1. 环境处于冻结状态时，先恢复它：挂起的进程处理不了关闭请求，窗口也是隐藏的。
2. 给环境里每个任务栏窗口发送关闭请求。GoLand 会正常保存后退出，需要确认时会照常弹窗；终端窗口关闭后，里面的 Claude Code、codex 会随之退出。
3. 最多等待 15 秒。进程全部退出后，环境回到「未运行」。
4. 超时后不会自动强制结束，而是列出没有退出的进程。通常是程序在等你确认，处理后再关闭一次即可。确实卡死的，用面板里的「强制结束…」或 `envfreeze kill`。

关闭只影响这个环境里的程序，其他环境和环境外的程序都不受影响。白名单进程也属于环境，会一起关闭。

也可以直接手动关掉环境里的所有程序，效果相同；冻结中的环境要先恢复再关。退出托盘不会关闭任何环境。

## 单实例程序

只有 envfreeze 启动的程序（及其子进程）才属于环境，手动打开的程序不受管理。单实例程序需要特别注意。

### JetBrains IDE（GoLand、IDEA 等）

JetBrains IDE 是单实例的：再次运行 `goland64.exe <项目>` 时，它只会把项目交给已在运行的实例，然后立即退出。`launch` 会检测这种"启动后 3 秒内退出"的情况并给出警告。

在面板里添加 IDE 时，「为本环境使用独立实例」默认勾选，会自动完成以下设置：

- 为环境生成独立的配置和缓存目录 `%APPDATA%\envfreeze\jetbrains\<环境>\`；
- 首次创建时复制现有设置（主题、快捷键、许可证等），插件目录直接共用；
- 设置 `GOLAND_PROPERTIES` 这类环境变量，让 IDE 使用上述目录。

这样每个环境都有自己的 IDE 进程，可以分别冻结。同一环境的多个项目在 `args` 中逐个列出，它们会在同一个实例里以多个窗口打开。隔离后的实例第一次启动时需要重新建立索引。

<details>
<summary>手动配置独立实例</summary>

1. 把 IDE 安装目录下的 `bin\idea.properties` 复制到例如 `%APPDATA%\envfreeze\goland\proj-a\idea.properties`，并在末尾追加：

   ```properties
   idea.config.path=${user.home}/.envfreeze-goland/proj-a/config
   idea.system.path=${user.home}/.envfreeze-goland/proj-a/system
   idea.plugins.path=${idea.config.path}/plugins
   idea.log.path=${idea.system.path}/log
   ```

2. 在配置中给该程序设置 `env = { GOLAND_PROPERTIES = '<上述文件路径>' }`。
3. 想沿用现有设置，可以把 `%APPDATA%\JetBrains\<产品版本>` 复制到新的 config 目录，或者使用 Settings Sync。

</details>

### Navicat

Navicat 同样是单实例程序，只能归属于一个环境。如果它已经被手动打开，请先关掉，再由环境启动。

### 终端

- 请直接配置 `pwsh.exe`、`powershell.exe` 或 `cmd.exe`，不要配置 `wt.exe`。Windows Terminal 是单实例的打包应用，从它启动的进程不会进入环境。
- Windows 11 默认会把新控制台转交给 Windows Terminal，那样窗口属于环境之外，冻结时无法隐藏。因此 envfreeze 会让控制台程序自动由 `conhost.exe` 承载：每个终端都有独立的窗口，并且归属于环境。系统的默认终端设置不需要修改。
- Claude Code、codex 等在这些终端里运行的工具，会自动随终端归入环境。

## 工作原理

```text
envfreeze launch A ──▶ envfreeze __hold A（后台 holder，不在 Job 内）
                          │ 创建并持有命名 Job：Local\envfreeze_A
                          │ CreateProcess(挂起) → 加入 Job → 恢复主线程
                          ▼
               GoLand / Navicat / pwsh ……（及它们派生的所有子进程）

envfreeze freeze A ──▶ 枚举 Job 内进程 → 隐藏窗口 → NtSuspendProcess → EmptyWorkingSet
envfreeze thaw   A ──▶ NtResumeProcess → 显示窗口 → 激活原前台窗口
```

- **Job Object**：每个环境对应一个命名 Job。程序以挂起状态创建、加入 Job 后才开始运行，因此它派生的任何子进程都在 Job 里。Job 不设置 `KILL_ON_JOB_CLOSE`，envfreeze 退出不会影响程序。
- **holder**：命名内核对象只在有句柄打开时存在。holder 一直持有 Job 的句柄，等环境里的进程全部退出后自行退出，几乎不占资源。
- **隐藏窗口**：隐藏要靠目标进程自己处理窗口消息，所以必须先隐藏、确认生效后再挂起；恢复时先恢复进程，再显示窗口。
- **冻结记录**：`%LOCALAPPDATA%\envfreeze\state\<环境>.frozen` 记录被挂起的进程（PID 和创建时间）和被隐藏的窗口。窗口隐藏前就写好记录，即使中途出错，`thaw` 也能把一切恢复原样。
- **防止重复冻结**：进程挂起是计数的，冻结两次就需要恢复两次。存在冻结记录时，`freeze` 会直接跳过。
- **容错**：单个进程失败时继续处理其余进程，最后汇总报错。`thaw` 失败时保留记录，可以放心重试。

日志位于 `%LOCALAPPDATA%\envfreeze\logs\`：

| 文件 | 内容 |
|---|---|
| `tray.log` | 托盘 |
| `panel.log` | 面板 |
| `<环境>.log` | 各环境的 holder |

## 已知限制

- **网络连接**：冻结期间网络连接可能超时断开，例如数据库连接，或者 Claude Code / codex 正在进行的流式请求。建议在程序空闲时冻结。
- **恢复后首次操作稍慢**：恢复本身是瞬间完成的，但内存要在被用到时才从压缩区或页面文件换入，切回后的头几秒可能有卡顿。
- **页面文件大小**：冻结不会减少提交内存（commit charge），页面文件需要足够大，能容纳所有冻结环境的私有内存。
- **未响应的程序**：冻结时程序本身正卡住的话，它的窗口可能来不及隐藏，冻结后会显示"未响应"。`freeze` 会提示这种情况。如果 Windows 弹窗询问是否关闭程序，请不要点「关闭程序」，否则进程会被结束。
- **任务管理器**：被冻结的进程仍会出现在任务管理器中，状态为"已挂起"，这是正常的。
- **权限**：请不要以管理员身份运行 envfreeze，否则它启动的所有程序都会以管理员身份运行。普通权限的 envfreeze 无法冻结或隐藏以管理员身份运行的程序，遇到时会给出明确提示。
- **可用内存的统计**：换出的页面会先进入修改列表或内存压缩区，所以"系统可用物理内存"可能要过几秒才会上升。各进程工作集的下降是即时、准确的。
- **在托管终端里执行 `launch`**：有些终端关闭时会结束它启动的所有进程。如果 holder 无法脱离这类终端，`launch` 会给出警告，此时关闭该终端可能连带结束整个环境。用托盘启动环境不受影响。

## 从源码构建

需要 Windows 和较新的稳定版 Rust（项目使用 Rust 2024 edition）。

```powershell
git clone https://github.com/florune/envfreeze
cd envfreeze
cargo build --release              # 产物在 target\release\
cargo install --locked --path .    # 或者直接安装
```

[.cargo/config.toml](.cargo/config.toml) 开启了 MSVC 运行库静态链接，编译出的 exe 可以直接拷到其他电脑上运行。

## 许可证

基于 [Apache License 2.0](LICENSE) 发布。
