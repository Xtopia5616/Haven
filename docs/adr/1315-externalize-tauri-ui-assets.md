# ADR 1315：将 Tauri 前端资源移出 Rust 可执行文件

## 背景

Vite 已将 CSS、JavaScript、字体和图像输出为独立文件，但 Tauri 的 `frontendDist` 目录模式仍会在 Rust 编译时压缩并把整个 `ui/build` 注入应用二进制。该步骤会扩大 Tauri context 宏的生成输入，也让前端资源不能作为独立文件检查。

## 决定

1. Tauri 安装包将 `ui/build` 复制到资源目录 `ui/`，应用通过 Tauri 内建的 `http://tauri.localhost` 协议从该目录提供页面与静态文件；运行时不依赖外网或额外 HTTP 服务。
2. `ResourceUiAssets` 实现 Tauri `Assets`，只接受资源根目录内的规范文件路径，拒绝路径穿越和越界符号链接；找不到 UI 资源时，生产启动失败并给出明确错误。
3. UI 构建后用现有 `jsdom` 解析 `index.html`，生成仅包含内联脚本 SHA-256 值的隐藏清单。运行时验证清单大小、条目数和哈希格式，并让 Tauri 将这些哈希加入 CSP；脚本策略保持不变，清单不会通过静态资源接口公开。
4. `tauri dev` 继续使用 Vite dev server；发布构建才从已打包的独立文件目录加载资源。

## 替代方案

- 继续把整个目录内嵌并压缩：拒绝；维持了当前 Rust 编译期资源压缩与注入工作。
- 用远端站点或本地 HTTP server 承载 UI：拒绝；远端依赖不满足离线桌面应用要求，本地 server 会增加监听端口、生命周期和 CSP/来源配置。
- 对内联脚本放宽 CSP：拒绝；改为在 UI 构建时计算脚本哈希，保留严格的脚本来源策略。

## 影响与回滚

MSI/NSIS 安装目录中的 UI 文件现在位于 `resources/ui`，Rust 可执行文件不再包含这些文件的压缩副本；安装包最终大小会受独立资源与安装器压缩方式影响。此改动减少 Tauri context 的资源压缩和注入工作，但不改变 Rust 依赖编译或 ThinLTO 的耗时，实际总构建提速需按目标机器测量。单独运行未打包的 Release exe 不再携带 UI，开发仍通过 `cargo tauri dev` 启动。回滚时恢复目录型 `frontendDist` 并移除自定义 asset provider 与 CSP 清单生成器；无需数据库或用户配置重置。

## 验收

- `corepack pnpm --dir ui run build` 生成 HTML、CSS、JavaScript 和 CSP 哈希清单。
- `cargo check --locked -p haven-app-binary --lib`、`cargo test --locked -p haven-app-binary ui_assets::tests`、Tauri Release bundle build 与 ADR 索引检查通过。
- 需要真实安装包验证资源路径和 WebView CSP；如果构建环境无法启动安装包，需在发布验收中记录该限制。
