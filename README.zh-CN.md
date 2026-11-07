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

- **终端**：多标签 + 任意方向分屏、标签拖拽分离为独立窗口、断线自动重连；真彩色 / Unicode 宽字符 / 鼠标上报 / 括号粘贴 / 搜索 / 超链接；终端间输入广播（可按服务器过滤）；ZMODEM（rz/sz）与 trzsz 终端内文件传输
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
| `list_sessions` | 列出会话档案（不含凭据） |
| `ssh_exec` | 在会话上执行 shell 命令，返回 stdout/stderr/exit code。命令按会话的访问档位过内置策略分类器；高风险命令弹出应用内确认框（调用可能返回 `pending_approval`，用相同参数重试即继续等待）。按会话限速，全部尝试落审计 |
| `sftp_home` / `sftp_list` / `sftp_stat` / `sftp_read` | 远端浏览与读文件 |
| `sftp_write` / `sftp_mkdir` / `sftp_delete` / `sftp_rename` / `sftp_chmod` | 远端写入与元操作（落审计） |
| `sftp_upload` / `sftp_download` | 文件传输：入队后台传输队列执行（与 UI 传输面板共享，**无大小上限**；`wait_seconds` 可同步等待终态） |
| `sftp_transfer_list` | 查询传输进度 / 状态 / 错误 |
| `terminal_open` / `terminal_send` / `terminal_read` / `terminal_close` / `terminal_list` | 交互式终端：在会话上开有状态 shell、发送输入、读取输出、关闭——用于 REPL、分页器等 `ssh_exec` 驱动不了的交互。窗口在线时以普通标签打开，可旁观、你键入时 AI 暂停写入、右键随时收回控制；也可右键把已有标签「共享给 AI」（独立的 `terminal` 权限组，**默认关闭**：按键字节流无法经过命令策略） |

**权限模型**（按 会话 → 最近的分组 → 全局 解析）：

1. 全局默认：设置 → MCP，6 个工具分组开关（list_sessions / ssh_exec / SFTP 读取 / SFTP 写入 / SFTP 传输 / 交互终端；交互终端默认关闭）与访问档位（默认只读 / 标准 / 完全）
2. 分组策略：分组右键 → MCP 策略，可设工具分组、访问档位与生产环境标记，子分组及其会话继承
3. 会话覆盖：会话编辑器 → MCP 权限页签，每项都可继承或覆盖——生产机禁 `ssh_exec`、测试机全开，互不干扰
4. 命令策略（恒开）：`ssh_exec` 命令执行前分类。只读档仅放行白名单；标准档无法证明只读的命令需确认；完全档仅黑名单需确认。黑名单 / 间接执行器 / 无法解析的结构在任何档位都需确认，生产环境会话的非只读命令也总是需要确认。确认走应用内弹窗（默认焦点在「拒绝」，5 分钟未决自动拒绝），没有可用的应用窗口时 fail-closed 拒绝；每会话限速（`mcp.rate_limit_per_minute`，默认 60 次/分钟）；所有尝试与决定写入审计日志

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
