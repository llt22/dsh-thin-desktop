# DSH Thin Desktop

[![CI](https://github.com/llt22/dsh-thin-desktop/actions/workflows/ci.yml/badge.svg)](https://github.com/llt22/dsh-thin-desktop/actions/workflows/ci.yml)
[![Release](https://github.com/llt22/dsh-thin-desktop/actions/workflows/release.yml/badge.svg)](https://github.com/llt22/dsh-thin-desktop/actions/workflows/release.yml)

一个用于更方便启动 DeepSeek Harness 的轻量桌面工具。

它不是另一套 DSH Desktop，也不内置 Node 或 DSH runtime。桌面端只负责：

- 查找用户本机的 Node 和 npx
- 启动用户现有环境中的 DSH Web
- 加载本地 DSH 页面
- 管理启动日志、重启和退出

桌面启动器可以保持稳定、少更新；DSH 版本由 `DSH_VERSION` 指定（默认钉在 `0.1.7-rc.1`），并继续使用用户已有的 profile、插件和配置。

## 当前实现

项目使用 Tauri 2：

- Rust 负责运行环境发现和 DSH 子进程生命周期
- 系统 WebView 展示启动状态和 DSH Web 页面
- DSH 页面中的外部 HTTP/HTTPS 链接使用系统默认浏览器打开
- 前端是无框架的静态 HTML、CSS 和 JavaScript
- 不包含 Electron、Node runtime 或第二份 DSH

默认启动等价于：

```sh
npx -y @deepseek-ai/dsh@0.1.7-rc.1 --profile web --host 127.0.0.1 --port 0
```

版本默认钉住，避免 npm `latest` 标签在两次启动之间漂移；换版本只需设置 `DSH_VERSION`，不必重新发布桌面应用。

## 本地环境发现

从 Finder 或 Dock 启动 GUI 应用时，进程通常拿不到终端中的 NVM、fnm 等环境。启动器会按以下顺序查找可用的 Node 和 npx：

1. `DSH_NPX` 指定的路径
2. 桌面进程当前的 `PATH`
3. 用户登录 shell 中的 `node`、`npx` 和 `PATH`
4. `NVM_BIN`
5. NVM、fnm、Volta、asdf、mise 的常见安装目录
6. Homebrew 和系统常见目录

找到工具后会执行 `node --version` 和 `npx --version`。当前要求 Node `>=22.19.0`，并会把 Node 所在目录放到 DSH 子进程的 `PATH` 最前面，避免绝对路径下的 npx 因找不到 node 而失败。

## 开发运行

需要：

- Node.js `>=22.19.0`
- npm
- Rust toolchain
- macOS 上的 Xcode Command Line Tools

```sh
cd dsh-thin-desktop
npm install
npm run dev
```

首次运行 DSH 时，npx 可能需要联网下载 npm 包。之后会使用 npm 自身的缓存。

## 验证与构建

运行 Rust 单元测试：

```sh
npm run check
```

构建桌面应用和安装包：

```sh
npm run build
```

macOS 产物位于：

```text
src-tauri/target/release/bundle/
```

## 下载版本

[GitHub Releases](https://github.com/llt22/dsh-thin-desktop/releases) 提供 Apple Silicon（`aarch64`）与 Intel（`x86_64`）两套 macOS 安装包。版本标签必须与 `package.json` 和 `src-tauri/tauri.conf.json` 中的版本一致，例如 `v0.2.3`。

macOS 安装包使用 Ad-hoc 签名，并在 Release 流水线中执行严格签名校验，避免不完整签名被误报为“应用已损坏”。由于没有使用付费 Apple Developer ID 和公证，Gatekeeper 首次打开时仍可能阻止运行。可以右键点击应用并选择“打开”，或下载仓库中的安装脚本后执行：

```sh
bash scripts/install-macos.sh ~/Downloads/DSH.Thin.Desktop_0.2.3_aarch64.dmg
```

脚本会挂载 DMG、将应用安装到 `/Applications`，并清除该应用的 quarantine 标记。

## 环境变量

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `DSH_VERSION` | `0.1.7-rc.1` | 启动的 DSH 版本；默认钉住，避免 `latest` 漂移 |
| `DSH_PROFILE` | `web` | 复用的 DSH profile |
| `DSH_HOST` | `127.0.0.1` | 仅允许 `127.0.0.1`、`localhost` 或 `::1` |
| `DSH_PORT` | `0` | `0` 表示自动分配空闲端口 |
| `DSH_NPX` | 自动查找 | 显式指定 npx 路径 |
| `DSH_EXECUTABLE` | 空 | 直接启动指定的 dsh 可执行文件，不走 npx |
| `DSH_EXTRA_ARGS` | 空 | 追加传给 DSH 的参数，支持引号；不能覆盖 `--host` 或 `--port` |

切换 DSH 版本：

```sh
DSH_VERSION=0.1.5-rc.3 npm run dev
```

指定 npx：

```sh
DSH_NPX="$HOME/.nvm/versions/node/v24.14.0/bin/npx" npm run dev
```

直接使用本地 dsh：

```sh
DSH_EXECUTABLE=/path/to/dsh npm run dev
```

这些变量只用于覆盖启动方式，不会修改用户的 Node、npm、DSH profile 或插件。

## 进程与安全边界

- DSH 使用独立进程组启动，重启和退出时会清理整个进程组
- 窗口关闭、应用退出、SIGINT 和 SIGTERM 都会触发同一套清理逻辑
- 重启会等待旧进程退出后再创建新进程，避免误杀和句柄竞态
- DSH 只允许绑定本机回环地址
- 仅接受 DSH 日志中指向回环地址的 HTTP/HTTPS URL
- 启动页拥有最小 Tauri IPC 权限；远程 DSH 页面未加入 capability 的 remote URL 范围
- 日志按行读取，避免 stdout 数据块拆分导致 URL 丢失

## 项目结构

```text
dsh-thin-desktop/
├── package.json
├── src/
│   ├── index.html
│   ├── app.js
│   └── styles.css
└── src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── capabilities/
    │   └── default.json
    └── src/
        ├── main.rs
        ├── discovery.rs
        └── launcher.rs
```

### `discovery.rs`

发现并验证用户本地 Node/npx，恢复 GUI 启动时缺失的登录 shell 环境。

### `launcher.rs`

构造 DSH 命令、管理进程组、读取日志、解析本地 URL、等待服务就绪，并提供诊断快照。

### `main.rs`

注册 Tauri 命令、启动后台任务，并在窗口关闭或应用退出时停止 DSH。

## 排障

### 找不到 npx

先在终端确认：

```sh
node --version
npx --version
```

如果终端正常但桌面端仍找不到，复制启动页的诊断信息。也可以临时设置 `DSH_NPX` 验证路径。

### Node 版本不支持

启动器会显示发现来源、实际版本和最低要求。通过用户自己的版本管理器切换到 Node 22.19 或更高版本后重启桌面端。

### DSH 下载或启动失败

启动页会保留 npx 和 DSH 的 stdout/stderr。常见原因包括网络不可用、npm registry 配置、profile 或插件加载失败。

### profile 不符合预期

默认使用官方 Web profile：

```text
~/.dsh/profiles/web
```

需要排查旧桌面 profile 时可以临时切换：

```sh
DSH_PROFILE=desktop npm run dev
```
