# mySSH

[English](README.md) | 简体中文

[![CI](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml/badge.svg)](https://github.com/xinchen2003/mySSH/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-0.5.0-blue)](https://github.com/xinchen2003/mySSH/releases)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-lightgrey)](https://github.com/xinchen2003/mySSH)

**本地优先的 Windows SSH 客户端，内置 MCP 服务端——把服务器直接暴露给 AI agent 操作。**

无需登录、无云端依赖、凭据不出本机。Tauri 2（Rust + WebView2）+ React。

## 为什么是 mySSH

- **AI 就绪**：内置 MCP server，Claude Code / OMP / OpenCode 等 agent 可直接在你保存的会话上执行命令、读写文件、传文件——权限粒度到「会话 × 工具组」，你说了算
- **本地优先**：配置存本机 SQLite，凭据进 Windows DPAPI 保险库，永不明文落盘
- **性能有预算**：终端 IPC 走二进制 Channel 而非 JSON 事件；隧道 / SFTP / 监控各占独立 SSH 连接，大流量不卡交互终端

## 功能

- **终端**：多标签 + 任意方向分屏、标签拖拽分离为独立窗口、断线自动重连；真彩色 / Unicode 宽字符 / 鼠标上报 / 括号粘贴 / 搜索 / 超链接；终端间输入广播（可按服务器过滤）；命令片段（命令面板插入，或键入触发词按 Tab 展开；多行片段以括号粘贴整体插入）；ZMODEM（rz/sz）与 trzsz 终端内文件传输；会话录像（右键「开始录像」；asciicast v2 格式，内置播放器或 asciinema 播放，可导出纯文本；键盘输入不录）
- **会话管理**：嵌套分组树、右键移动到分组、标签、收藏、模糊搜索、命令面板（Ctrl+Shift+P）；密码 / 公钥（OpenSSH + .ppk）/ keyboard-interactive（2FA）/ agent 认证；多级跳板机；known_hosts 首连与密钥变更确认；登录宏（认证进 shell 后逐行自动执行命令）；su 二级登录（普通账号登入后自动切 root，密码存保险库、提示出现时自动应答一次）
- **本地会话**：ConPTY 承载 PowerShell / pwsh / CMD，与 SSH 会话并列；自定义启动目录与启动命令（如连接即拉起 AI agent CLI）
- **隧道**：本地 / 远程 / 动态 SOCKS5 转发，开机自启、随会话建立、断线自动恢复；独立 SSH 连接与交互终端隔离
- **SFTP**：双栏文件管理器、双向拖拽（含 OS 文件/文件夹直接拖入）、队列化传输（并发控制 / 断点续传 / 失败重试）、跨会话传输历史、远程文件直编（保存自动回传）、跟随终端当前目录（OSC 7）
- **监控**：CPU / 内存 / 磁盘 / 网络实时图表（独立采样通道，失败静默降级）
- **导出**：明文或口令加密（Argon2id + AES-256-GCM）配置包
- **主题与外观**：8 套内置配色（One Dark / Solarized / Nord / 黑夜 / GitHub / 护眼绿 / 暖阳）、图形化自定义主题编辑器（告别手写 JSON）、自定义终端背景图与不透明度
- **多语言**：简体中文 / English，设置中切换
- **终端编码**：按会话配置（UTF-8 / GBK / GB18030 / Big5 / Shift_JIS / EUC-KR），流式转码，UTF-8 零拷贝直通
- **自动更新**：启动时静默检查 GitHub Releases，发现新版本确认后自动下载安装并重启；一键导出诊断包（日志 + 系统信息打包 zip），提 issue 直接附上

## AI 自动化（MCP）

内置 MCP 服务端（Streamable HTTP，仅监听 127.0.0.1 + Bearer 令牌），agent 配置在设置面板一键复制。

**18 个工具**：

| 工具 | 说明 |
| --- | --- |
| `list_sessions` | 列出会话档案（不含凭据），并附上每个会话对 AI 的有效授权（档位、生产标记、各工具组是否放行）、已打开的标签数，以及 AI 可直接使用的终端（含你共享给 AI 的标签） |
| `ssh_exec` | 在会话上执行 shell 命令，返回 stdout/stderr/exit code。命令按会话的访问档位过内置策略分类器；高风险命令弹出应用内确认框（调用可能返回 `pending_approval`，用相同参数重试即继续等待）。按会话限速，全部尝试落审计。输出省 token：剥离 ANSI 序列，超长输出保留头尾。窗口在线时，命令、实时输出和退出码同步显示在该会话终端标签下方的只读分屏里（会话未打开时自动打开；设置 → MCP →「AI 命令镜像」可关闭） |
| `sftp_home` / `sftp_list` / `sftp_stat` / `sftp_read` | 远端浏览与读文件 |
| `sftp_write` / `sftp_mkdir` / `sftp_delete` / `sftp_rename` / `sftp_chmod` | 远端写入与元操作（落审计） |
| `sftp_upload` / `sftp_download` | 文件传输：入队后台传输队列执行（与 UI 传输面板共享，**无大小上限**；`wait_seconds` 可同步等待终态） |
| `sftp_transfer_list` | 查询传输进度 / 状态 / 错误 |
| `file_view` / `file_edit` / `file_grep` / `file_glob` | 远程文件：带行号分页查看、精确字符串替换编辑（唯一匹配 + etag 防并发覆盖）、正则搜索、glob 查找——全部经 SFTP，不执行远端命令 |
| `file_backups` / `file_rollback` | AI 改动自动备份与回滚：`file_edit` / `sftp_write` / `sftp_delete` 改前自动备份原内容；文件在改动后又被他人修改时默认拒绝回滚。每条记录带 AI 可选填写的标题（`title`）和程序算出的改动摘要；侧边栏会话右键「AI 文件改动…」可一键回滚。备份默认保留 7 天、审计日志 30 天，可在「设置 → 通用 → 历史记录保留」调整 |
| `recipe_list` / `recipe_run` | 内置运维 recipe：系统概况、磁盘占用、进程排行、端口、服务状态/日志、Docker 容器、服务重启、发信号等 16 个参数化模板，各带风险等级（read / change / danger）。参数按白名单校验后代入，read 级在只读档也能用（如 `docker ps`），danger 级始终需你确认 |
| `job_start` / `job_read` / `job_stop` / `job_list` | 后台命令：长时间运行的命令（构建、`tail -f`、长下载）后台启动立即返回，按字节游标分段读取输出（默认剥离 ANSI 控制序列、折叠进度条），可随时停止；命令门与 `ssh_exec` 相同，属 `ssh_exec` 权限组 |
| `terminal_open` / `terminal_send` / `terminal_read` / `terminal_close` / `terminal_list` | 交互式终端：在会话上开有状态 shell、发送输入、读取输出、关闭——用于 REPL、分页器等 `ssh_exec` 驱动不了的交互。`terminal_read` 可在服务端等待提示符正则（`wait_for`）或输出静默（`idle_ms`），判定命令执行完毕而无需反复轮询。窗口在线时以普通标签打开，可旁观、你键入时 AI 暂停写入、右键随时收回控制；AI 关闭自己打开的终端时标签一并关闭（你共享的标签保留并提示）；也可右键把已有标签「共享给 AI」（独立的 `terminal` 权限组，**默认关闭**：按键字节流无法经过命令策略）。AI 控制的终端全程自动录像（含 AI 输入的命令），录像结束时其 SHA-256 写入哈希链审计日志，可在命令面板 →「会话录像」校验录像与审计日志事后未被改动 |

**权限模型**（按 会话 → 最近的分组 → 全局 解析）：

1. 全局默认：设置 → MCP，6 个工具分组开关（list_sessions / ssh_exec / SFTP 读取 / SFTP 写入 / SFTP 传输 / 交互终端；交互终端默认关闭）与访问档位（默认只读 / 标准 / 完全）
2. 分组策略：分组右键 → MCP 策略，可设工具分组、访问档位与生产环境标记，子分组及其会话继承
3. 会话覆盖：会话编辑器 → MCP 权限页签，每项都可继承或覆盖——生产机禁 `ssh_exec`、测试机全开，互不干扰
4. 命令策略（恒开）：`ssh_exec` 命令执行前分类。只读档仅放行白名单；标准档无法证明只读的命令需确认；完全档仅黑名单需确认。黑名单 / 间接执行器 / 无法解析的结构在任何档位都需确认，生产环境会话的非只读命令也总是需要确认。确认走应用内弹窗（默认焦点在「拒绝」，5 分钟未决自动拒绝）：用一句话总结命令做了什么（heredoc 正文按数据处理，写入的文件与 sudo 会点明），并说明需要确认的原因，同时显示 AI 传入的 `purpose` 用途说明（标注为未经验证）；选「总是允许」后，同一会话的完全相同命令不再询问，重启 mySSH 后失效。弹窗醒目标出命令所在的会话（色点、名称、user@host、分组，生产环境红色标记），可一键跳到该会话的终端标签（没有则新开）；多个会话同时有待确认项时，左栏按会话分组列出，可「拒绝本会话全部」，允许必须逐条。窗口不在前台时闪任务栏。工具栏「AI」按钮打开 AI 活动面板，按会话列出待确认、执行中命令、后台命令和 AI 终端，每项可一键停止；标签栏和侧边栏也会显示「AI 执行中 n」「待确认 n」徽标。没有可用的应用窗口时 fail-closed 拒绝；每会话限速（`mcp.rate_limit_per_minute`，默认 60 次/分钟）；所有尝试与决定写入审计日志

配置示例（`.omp/mcp.json`，Claude Code 同构）：

```json
{
  "mcpServers": {
    "myssh": {
      "type": "http",
      "url": "http://127.0.0.1:17345/mcp",
      "headers": { "Authorization": "Bearer <令牌>" }
    }
  }
}
```

## 路线图

- Agent CLI：可脚本化的命令行接口（规划中）
- 堡垒机导航：自动操作菜单式堡垒机（如 JumpServer）到达目标资产（规划中）

## 技术栈

| 层       | 选型                                                |
| -------- | --------------------------------------------------- |
| 桌面     | Tauri 2.11（Rust + WebView2）                        |
| 前端     | TypeScript + React 19 + Vite + zustand + Tailwind 4 |
| 终端     | xterm.js 6 + WebGL addon（canvas 兜底）              |
| SSH      | russh 0.62 / russh-sftp 2.4                         |
| 本地终端 | portable-pty（ConPTY）                              |
| 存储     | SQLite（sqlx）+ DPAPI 凭据保险库                     |
| 异步     | tokio                                               |

## 构建

要求：Rust stable、Node.js 20+、Windows 10/11（WebView2 Runtime）。

```bash
# 前端
cd app/ui
npm install
npm run build

# 桌面应用（release 二进制）
cargo build -p app --release
# 产物：target/release/app.exe

# 安装包（NSIS + MSI）——仓库根目录执行
app/ui/node_modules/.bin/tauri.cmd build
# 产物：target/release/bundle/
```

开发模式（热更新）：

```bash
cd app/ui && npm run dev        # Vite dev server
cargo run -p app                # 另开一个终端
```

## 测试与质量

```bash
# Rust（仓库根）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 前端（app/ui）
npm run lint
npm test
```

工程红线：非测试代码禁 `unwrap`/`expect`（豁免须附 `// SAFETY:` 注释）、禁 `unsafe`；终端输出走 Tauri IPC Channel（二进制，非 JSON 事件）；隧道 / SFTP 各占独立 SSH 连接，与交互终端传输层隔离。

## 项目结构

```
crates/
  app/          Tauri 壳：命令层、IPC 装配
  core-ssh/     SSH 协议核心（连接、认证、通道）
  core-tunnel/  端口转发
  core-sftp/    SFTP 与传输队列
  core-monitor/ 服务器监控采样
  core-store/   SQLite 持久化、凭据保险库、导入导出
  core-policy/  策略
  cli/          Agent CLI（规划中）
app/ui/         前端（React + xterm）
```

## 数据位置

`%LOCALAPPDATA%\myssh\`：`myssh.db`（SQLite：会话、隧道、传输历史）、凭据（DPAPI 加密，绑定当前 Windows 用户与机器）、`known_hosts`、`logs/`。

## 许可证

[MIT License](LICENSE)
