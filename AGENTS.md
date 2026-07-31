# ProxyBar 开发注意事项与准则

本文件适用于整个仓库。开发前先阅读本文件、`README.md` 和 `需求文档.md`。
`需求文档.md` 是原始需求背景，当前实现已经有部分调整；发生冲突时以当前代码和用户最新要求为准，完成行为变更后同步更新文档。

## 1. 项目定位

ProxyBar 是基于 Tauri 2 的 macOS 菜单栏 / Windows 托盘代理客户端：

- 应用启动后没有常驻主窗口，只显示托盘图标。
- Rust 后端负责托盘菜单、设置持久化、订阅解析、sing-box 生命周期和 TUN 编排。
- TypeScript/Vite 前端只负责“设置”窗口，不承载代理核心逻辑。
- 当前正式支持 macOS 和 Windows；Linux 尚未实现。
- 只解析 Loon 格式订阅中的 VLESS 节点。

当前代理模式：

- 关闭：停止本应用启动的 sing-box，不创建 TUN。
- 手动：启动 sing-box 的本地 SOCKS5 inbound，不创建 TUN。
- 自动：启动 sing-box TUN，私网和中国大陆流量直连，其余流量代理。
- 全局：启动 sing-box TUN，必要的私网流量直连，其余流量代理。

## 2. 目录职责

```text
src/                          设置窗口（TypeScript/CSS）
src-tauri/src/                Rust 后端
src-tauri/icons/              应用和安装包图标的唯一来源
assets/common/                所有平台共享的运行时资源
assets/platforms/macos/       macOS x86_64 / arm64 sing-box
assets/platforms/windows/     Windows x64 sing-box 及运行库
scripts/build.mjs             平台构建与产物整理
.build/                       所有中间构建文件
release/macos/                最终 macOS 产物
release/windows/              最终 Windows 产物
```

Rust 模块职责：

- `lib.rs`：应用启动、托盘菜单、命令、状态和主要业务编排。
- `model.rs`：设置、运行时状态、节点及路径模型。
- `proxy.rs`：sing-box 进程、权限提升和生命周期。
- `sing_box.rs`：sing-box JSON 配置、TUN、DNS 和路由规则生成。
- `subscription.rs`：Loon VLESS 订阅解析。
- `localization.rs`：语言识别和外部语言文件加载。

不要把代理、文件或平台逻辑移入 `src/`。前端通过 Tauri command 与 Rust 通信。

## 3. 静态资源规范

资源结构必须保持平台平级：

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

规则：

- 不要重新创建 `assets/bin/`、`assets/windows/` 或平台内的 `bin/`、`scripts/` 子目录。
- 应用图标只放在 `src-tauri/icons/`。不要在 `assets/` 再保存一份应用图标。
- `assets/common/tray/` 只保存运行时状态图标，它们与应用图标职责不同。
- sing-box 二进制和所需运行库由 `scripts/fetch-sing-box.mjs` 下载到对应的 `assets/platforms/<os>/` 根目录，但必须保持 Git ignore。macOS Universal 2 包同时准备 `sing-box-x86_64` 和 `sing-box-aarch64`，运行时按架构选择。
- sing-box GPLv3 许可证从同一官方发布包提取到 `assets/common/licenses/`，保持 Git ignore，但必须随最终包分发。
- sing-box 版本、官方文件名和 SHA-256 只在 `scripts/fetch-sing-box.mjs` 维护；下载必须校验哈希，压缩包与解压中间文件只能放在 `.build/`。
- 公共资源由 `tauri.conf.json` 打包到 `assets/common/`。
- 平台配置把当前平台目录统一映射到包内 `assets/platform/`；Rust 运行时不应依赖包内的 `macos` 或 `windows` 目录名。
- 修改资源目录后，必须同时检查 Tauri 配置、`resolve_paths` 和最终安装包内容。

## 4. 应用图标与托盘图标

- `src-tauri/icons/icon.png` 是应用图标源图；ICNS、ICO 和尺寸 PNG 也统一放在该目录。
- macOS 托盘图标必须使用 template image，以自动适配浅色和深色菜单栏。
- 创建托盘时保持 `.icon_as_template(cfg!(target_os = "macos"))`。
- 动态更新托盘图标时必须使用 `set_icon_with_as_template`，不能只调用 `set_icon`，否则 macOS 会丢失 template 属性，图标可能在深色菜单栏中不可见。
- 托盘状态 PNG 应使用透明背景和清晰的单色 alpha 轮廓。

## 5. 托盘菜单规则

- Tauri 的 `CheckMenuItem` 是复选项，不是 radio group。
- 代理模式必须通过应用逻辑显式取消其他模式的勾选，确保始终单选。
- 异步代理切换可能较慢，菜单勾选状态应立即更新，不能等 sing-box 启停完成后才更新。
- “设置”是顶层菜单项，不要重新放回“节点”子菜单。
- “刷新订阅”保留在“节点”子菜单顶部。
- 菜单发生结构或状态变化后，通过统一的 `rebuild_tray` 更新菜单、图标和 tooltip。

## 6. macOS 窗口注意事项

- 应用设置了 `LSUIElement=true` / Accessory activation policy，不显示 Dock 图标是预期行为。
- 设置窗口默认隐藏，关闭窗口时只隐藏，不销毁 WebView。
- 每次打开设置窗口都要重新读取后端状态，避免取消后再次打开仍显示未保存内容。
- 取消按钮通过 Rust command 隐藏窗口，不要仅依赖前端 Window API。
- `Esc` 应与取消按钮执行相同操作。
- 设置窗口使用 macOS 风格纯色：浅色背景 `#ECECEC`，深色背景 `#282828`。不要恢复蓝色渐变窗口背景。
- 系统强调色可以用于主按钮、焦点边框和小面积品牌元素。

## 7. 设置与兼容性

设置保存在用户数据目录的 `settings.json`：

- macOS：`~/Library/Application Support/ProxyBar/`
- Windows：`%LOCALAPPDATA%\ProxyBar\`

当前设置包括：

- 订阅地址。
- 固定本地 SOCKS5 端口，默认 `10850`。
- 当前节点。
- 当前代理模式。

规则：

- `Settings` 必须保留 `#[serde(default)]`，新增字段必须提供默认值，确保旧 `settings.json` 可继续读取。
- 旧版 `settings.conf` 迁移逻辑不能随意删除。
- 不内置默认订阅地址，并允许用户保存空地址。订阅地址为空时启动后打开设置窗口，代理模式只能为关闭，其他模式和刷新订阅必须禁用。
- 清空订阅地址时要立即切换到关闭模式，停止本应用的 sing-box 并删除 TUN。
- 端口范围为 `1024..=65535`，变更时先检查占用。
- 当前端口是用户指定的固定端口，不再自动递增寻找其他端口。
- 修改活动模式使用的端口后，要停止旧 sing-box，再用新端口启动。
- 修改订阅地址后刷新订阅。
- 不要把用户配置、sing-box 配置、规则缓存或日志写入仓库或应用资源目录。

## 8. sing-box 和 TUN 生命周期

- 应用只管理自己启动并保存在 `Runtime.process` 中的 sing-box 进程。
- 禁止使用 `killall sing-box`、`pkill sing-box` 等方式结束系统中所有 sing-box。
- macOS TUN 进程以管理员权限启动，并保存、核对精确 PID；手动模式保持普通用户子进程。
- Windows 应用通过 UAC 以管理员权限启动；sing-box 子进程使用隐藏控制台并以控制台中断优雅退出。
- 活动模式下必须监控受管 sing-box 进程和本地 SOCKS5 端口；异常退出或端口失效时在 `transition` 锁内按退避策略自动重启，用户主动切换或退出不得被旧的重启任务覆盖。
- 切换活动模式前先停止旧进程，再启动新进程。
- 退出、切换到关闭/手动模式或恢复错误状态时，必须停止原 TUN 进程，让 sing-box 清理虚拟网卡和路由。
- 模式切换必须通过 `transition` 锁串行化，避免同时启动多个 sing-box 或交叉修改路由。
- 不要在持有异步状态锁时执行长时间网络请求或等待子进程。
- 启动后必须检查本地 SOCKS5 端口，不能只凭进程创建成功判断模式已可用。

## 9. TUN、DNS 与路由规则

- TUN inbound、自动路由、DNS 劫持、GeoIP/Geosite 下载和缓存都由 sing-box 完成。
- Rust 不生成 PAC，不修改系统 SOCKS/PAC 设置，也不自行下载或解析 gfwList。
- 自动模式使用 sing-box 官方中国大陆 GeoIP/Geosite 二进制规则集：中国大陆和私网直连，其余代理。
- DNS 服务器选择必须由流量路由策略生成，不能维护另一份域名分类：代理域名通过当前节点查询 `8.8.8.8:853`（DoT），直连域名使用当前系统 DNS。macOS 必须把当前系统 DNS 的精确地址加入 TUN 路由，避免局域网 DNS 绕过劫持；不要把本地 DNS `detour` 到空配置的直连出站，sing-box 自身拨号应依靠自动网卡识别绕过 TUN。
- 全局模式除防止路由环路所需的直连及私网外，其余流量均代理。
- `route.auto_detect_interface` 必须保持启用，避免代理服务器连接再次进入 TUN。
- 修改 `sing_box.rs` 路由行为时必须补充配置单元测试，并用捆绑的 sing-box 执行 `check`。

## 10. 订阅与节点

- 只支持 VLESS，忽略损坏行和不支持的协议。
- 当前支持 TCP、WebSocket、gRPC、TLS/SNI、WS path/Host 和 gRPC service name。
- 刷新订阅得到空节点列表时应报错，不覆盖已有可用缓存。
- 选择的节点不存在时回退到第一个节点，并持久化新选择。
- 修改解析行为时必须补充 `subscription.rs` 和/或 `sing_box.rs` 测试。

## 11. 国际化

- 语言文件位于 `assets/common/locales/`。
- `en.conf` 是英语兜底，`zh-CN.conf` 是简体中文。
- 中文系统可能返回 `zh-Hans-SG`、`zh-SG` 等 locale，必须继续回退到简体中文文件。
- 新增可见文字时同时增加 Rust 默认值、英文语言文件和中文语言文件。
- 不要在前端复制固定中文 UI 文案；前端应使用后端提供的本地化文本。
- 现有少量错误信息仍是中文；新增或集中修改错误处理时应优先将其纳入语言文件。

## 12. 构建与产物

中间文件只能放在 `.build/`，最终产物只能放在：

```text
release/macos/ProxyBar-Intel.app
release/macos/ProxyBar-Apple-Silicon.app
release/macos/ProxyBar-Universal.app
release/windows/ProxyBar-Setup.exe
```

规则：

- 不要直接修改 `release/` 内文件；始终通过构建脚本重新生成。
- `npm run mac` 只能在 macOS 上执行。一次 Universal 构建应生成 Intel、Apple Silicon 和 Universal 2 三个应用包，不要重复执行三次完整编译。
- 仅需本机 Intel 验证时可运行 `npm run mac:x64`，只生成 `ProxyBar-Intel.app`。
- `npm run windows` 只能在 Windows x64 + MSVC + NSIS 环境执行。
- `dev`、`check`、`test` 和平台构建会自动下载当前主机/目标所需的 sing-box；可用 `SING_BOX_DOWNLOAD_PROXY` 指定下载代理。
- macOS 上的 `cargo-xwin` 可用于实验性交叉编译检查，但不能替代 Windows 上的正式 NSIS 构建和运行验证。
- 不要在没有成功产物时声称 Windows 安装包已经生成。
- macOS 当前使用 ad-hoc 签名，仅适合本地开发；正式分发需要 Developer ID 签名和 notarization。
- 构建脚本会清理并拍平对应的 `release/<platform>/` 目录，不要在其中存放手工文件。
- `src-tauri/gen/` 是生成目录，构建后可以清理，不应作为业务源码维护。

## 13. 修改后的最低验证要求

普通代码修改至少运行：

```bash
npm run check
npm test
```

涉及资源、Tauri 配置、平台路径或打包逻辑时，还必须在对应平台运行完整构建：

```bash
npm run mac
npm run windows
```

macOS 构建后检查：

- 三个 `.app` 均通过 `codesign --verify --deep --strict`。
- Intel 包的主程序和 sing-box 仅为 `x86_64`，Apple Silicon 包仅为 `arm64`，Universal 包主程序包含 `x86_64` 和 `arm64`。
- 三个包内都只有 `assets/common/` 和 `assets/platform/`，且不包含 Windows 的 `.exe`、`.ps1` 或目录。
- Intel 和 Apple Silicon 包只携带对应架构的 sing-box；Universal 包携带两份并按运行架构选择。
- 应用能启动，托盘图标可见，中文菜单正常。

Windows 构建后检查：

- `release/windows/` 只有 `ProxyBar-Setup.exe`。
- 安装、UAC、启动、托盘菜单、设置窗口、sing-box、TUN 和路由恢复均在真实 Windows 环境验证。
- 安装包只包含 Windows 平台资源，不包含 macOS sing-box。

## 14. 代码风格

- Rust 使用 `cargo fmt`，错误使用 `anyhow` 增加必要上下文。
- 异步文件、网络和进程操作优先使用 Tokio API。
- TypeScript 保持 strict 类型检查，不使用无必要的 `any`。
- 平台差异集中在平台配置、`proxy.rs` 和资源目录，不要把大量 `cfg` 分散到无关模块。
- 修改功能时补充针对核心纯逻辑的单元测试，不依赖真实网络完成测试。
- 保持托盘应用轻量，不引入大型前端框架或常驻主窗口，除非用户明确要求。
- 不恢复已删除的 Native SDK、Zig、旧打包脚本或旧目录结构。

## 15. 增加 Linux 时

Linux 不是复制一个目录即可完成。至少需要：

1. 新增 `assets/platforms/linux/`，直接放置 Linux sing-box。
2. 新增 `src-tauri/tauri.linux.conf.json`，映射到包内 `assets/platform/`。
3. 在 `proxy.rs` 实现 Linux TUN 权限和生命周期，不能继续返回 unsupported。
4. 在 `scripts/build.mjs` 增加 Linux 构建和拍平的 `release/linux/` 产物。
5. 确定支持的桌面环境、托盘协议、TUN 权限模型和路由工具，并在目标发行版实测。
6. 更新 README、需求文档、CI 和本文件中的平台说明。

在 Linux 实现完成前，不要创建空的 Linux 目录或声称项目已支持 Linux。
