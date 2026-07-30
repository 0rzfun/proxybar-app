# ProxyBar

ProxyBar 是一个基于 Tauri 2 的 macOS 菜单栏 / Windows 托盘代理切换工具。应用没有常驻主窗口，启动后只保留托盘图标；Rust 后端负责订阅和 sing-box 生命周期，Web 前端用于编辑应用设置。

download： [https://0rzfun.github.io/proxybar-app](https://0rzfun.github.io/proxybar-app)
## 功能

- 四种代理模式：
  - 关闭：停止本应用启动的 sing-box，不创建 TUN。
  - 手动：启动 sing-box 的本地 SOCKS5 inbound，不创建 TUN。
  - 自动：创建 TUN，中国大陆和私网流量直连，其余流量通过 VLESS 节点。
  - 全局：创建 TUN，除私网及必要直连外的流量全部通过 VLESS 节点。
- 本地 SOCKS5 端口可在“设置”中配置，默认使用 `10850`；端口被占用时会提示用户。
- 解析 Loon 格式的 VLESS 节点，支持 TCP、WebSocket、gRPC、TLS/SNI、WS path/Host 和 gRPC service name。
- 不内置订阅地址；首次启动或订阅地址为空时自动打开设置窗口。订阅地址支持缓存、刷新、修改和清空。
- 自动模式使用 sing-box 官方 GeoIP/Geosite 规则集，并由 sing-box 下载和缓存。
- 代理模式和当前节点会持久化；重新启动时恢复上次状态。
- 托盘图标随关闭、手动、自动、全局模式变化。
- macOS 同时生成 Intel、Apple Silicon 和 Universal 2 三种应用包；构建产物内置对应架构的官方 sing-box。Windows 构建产物内置 x64 sing-box、所需运行库及许可证。
- 菜单和设置窗口支持外部语言配置，内置简体中文和英语。
- 退出时停止 sing-box，由 sing-box 清理 TUN 和自动路由。

## 托盘菜单

```text
代理模式
├── 关闭
├── 手动
├── 自动
└── 全局
节点
├── 刷新订阅
└── 节点列表
设置
退出
```

“设置”窗口可以修改订阅地址和固定本地 SOCKS5 端口。点击取消、关闭窗口或按 `Esc` 都不会保存修改。

订阅地址允许留空。未设置订阅地址时应用只能使用“关闭”模式，“手动”“自动”“全局”和“刷新订阅”会显示为禁用状态；清空并保存订阅地址时会立即停止本应用的 sing-box 并删除 TUN。

## 技术结构

- `src-tauri/src/`：Rust 后端、托盘菜单和平台能力。
- `src-tauri/icons/`：应用和安装包图标的唯一来源，包括 PNG、ICNS 和 ICO。
- `src/`：设置窗口。
- `assets/common/`：所有平台共享的托盘状态图标、语言和许可证。
- `assets/platforms/<platform>/`：构建时下载、但不纳入 Git 的 sing-box 二进制和运行库。
- `src-tauri/tauri.macos.conf.json` / `tauri.windows.conf.json`：按平台打包对应的 sing-box 资源，避免互相混入安装包。

```text
assets/
├── common/
│   ├── licenses/
│   ├── locales/
│   └── tray/
└── platforms/
    ├── macos/
    │   ├── sing-box-aarch64
    │   └── sing-box-x86_64
    └── windows/
        ├── libcronet.dll
        └── sing-box.exe
```

打包时共享资源保持为 `assets/common/`，当前平台资源统一映射为 `assets/platform/`。Linux 当前未实现；增加 Linux 还需要平台代理逻辑、资源、Tauri 配置、构建脚本和真实桌面环境验证。

sing-box 二进制、Windows `libcronet.dll` 和许可证都不纳入 Git。`dev`、`check`、`test` 和平台构建命令会从官方 GitHub Release 下载所需目标，校验固定 SHA-256 后提取到上述资源目录；下载压缩包缓存于 `.build/sing-box-downloads/`。版本、文件名与校验值集中在 `scripts/fetch-sing-box.mjs`，升级时只需更新该文件。

构建中间文件统一放在根目录的 `.build/`，最终可分发产物只放在：

```text
release/
├── macos/
└── windows/
```

运行数据继续保存在原目录：

- macOS：`~/Library/Application Support/ProxyBar/`
- Windows：`%LOCALAPPDATA%\ProxyBar\`

旧版 `settings.conf` 会自动迁移；新版设置写入 `settings.json`。

主要运行文件包括订阅缓存、生成的 sing-box 配置、规则缓存、日志和 macOS 管理员进程 PID，它们都保存在用户数据目录，不会写入应用安装目录。

## 开发环境

- Node.js 22+
- Rust stable
- macOS 10.15+，或 Windows 10/11 x64
- Windows 构建需要 Visual Studio Build Tools（Desktop development with C++）和 WebView2

安装依赖：

```bash
npm install
```

需要为下载使用代理时设置 `SING_BOX_DOWNLOAD_PROXY`，例如 `socks5h://127.0.0.1:10850`。

开发运行：

```bash
npm run dev
```

## 检查与测试

```bash
npm run check
npm test
```

测试覆盖设置迁移、语言匹配、Loon VLESS 解析和 sing-box SOCKS/TUN 配置；测试还会使用当前平台捆绑的 sing-box 校验所有模式的配置。

## 在线构建与下载页

提交到 `master` 分支后，GitHub Actions 会自动在 GitHub 托管的 macOS 和 Windows 环境构建应用，并生成以下四个 ZIP：

```text
ProxyBar-macOS-Intel.zip
ProxyBar-macOS-Apple-Silicon.zip
ProxyBar-macOS-Universal.zip
ProxyBar-Windows-x64.zip
```

工作流会分别检查四个软件包。单个平台或单个软件包失败时记录 warning，不阻止其他成功软件包部署到 GitHub Pages，也不阻止更新标签为 `continuous` 的“ProxyBar 最新自动构建”预发布 Release；只有四个软件包全部失败时工作流才标记为失败并停止发布。Pages 和 Release 只提供本次实际构建成功的软件包，下载页会把缺失的软件包标记为不可用。Windows ZIP 是便携版，解压后运行 `ProxyBar.exe`。

首次使用时，需要在 GitHub 仓库的 **Settings → Pages → Build and deployment → Source** 中选择 **GitHub Actions**。工作流也可以在仓库的 **Actions → Build and publish downloads** 页面手动运行。

## 构建 macOS

在 Intel 或 Apple Silicon macOS 上运行：

```bash
npm run mac
```

只构建和验证 Intel 包时运行 `npm run mac:x64`；该命令不会编译 arm64 或 Universal 目标。

产物：

```text
release/macos/
├── ProxyBar-Intel.app
├── ProxyBar-Apple-Silicon.app
└── ProxyBar-Universal.app
```

构建命令只执行一次 Tauri Universal 构建：分别编译一次 `x86_64-apple-darwin` 和 `aarch64-apple-darwin`，再从同一份构建结果生成 Intel、Apple Silicon 和 Universal 2 三个 `.app`，最后分别执行 ad-hoc codesign。首次构建前需要安装两个 Rust target：

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
```

应用设置了 `LSUIElement=true`，不会显示 Dock 图标。Intel 和 Apple Silicon 单架构包只携带对应架构的 sing-box；Universal 包同时携带两个架构的 sing-box，并按当前运行架构自动选择。

## 构建 Windows x64

在 Windows x64 开发环境中运行：

```bash
npm run windows
```

产物：

```text
release/windows/ProxyBar-Setup.exe
```

Tauri 会生成 Windows x64 NSIS 安装包。Windows 版启动时请求 UAC 管理员权限，以便 sing-box 创建 TUN 和自动路由。

## 修改或新增语言

语言文件位于 `assets/common/locales/`：

- `en.conf`：英语兜底。
- `zh-CN.conf`：简体中文。

可以修改等号右侧文字。新增语言时复制文件并使用系统区域名，例如 `ja.conf` 或 `fr-FR.conf`。开发时可设置 `PROXYBAR_LOCALE=en` 强制语言。

打包后文件位于应用资源目录的 `assets/common/locales/` 中。

## 代理实现

- sing-box 始终提供监听 `127.0.0.1` 的 SOCKS5 inbound；只有自动和全局模式额外创建 TUN。
- TUN 使用 `auto_route`、`strict_route`、DNS 劫持和自动网卡识别，使 CLI 与不读取系统代理的应用也能被覆盖。
- 自动模式由 sing-box 的中国大陆 GeoIP/Geosite 规则集分流；全局模式不加载区域规则。
- macOS 仅在进入自动或全局模式时弹出管理员授权，应用以精确 PID 管理提升权限的 sing-box。
- Windows 应用通过 UAC 提升权限；sing-box 控制台保持隐藏并通过控制台中断优雅退出。
- 本地端口是用户配置的固定端口；端口被占用时提示错误，不自动切换到其他端口。
- PAC、gfwList、`networksetup`、PowerShell WinINET 脚本和系统 SOCKS 设置均不再使用。
