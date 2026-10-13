# 1154：在 WebView2 启动前显示原生加载画面并处理失败

## 状态

已接受并实现（2026-10-13）。

## 背景

`app.html` 中的 `#haven-boot` 依赖 WebView2 已创建并开始绘制网页。WebView2 冷启动和首次导航之前，主窗口只能显示背景色，用户无法区分加载与卡住。Tauri 的 HTML splash screen 仍需创建另一个 WebView2，不能覆盖 WebView2 自身的初始化间隙。

## 决定

- Windows 正常启动时，在启动 Tauri 主窗口前创建轻量原生 popup。它以 GDI 绘制深色背景和与 Haven `VoiceBars` 同节奏、同尺寸的三条浮动柱；独立消息循环只在窗口可见时重绘。
- 主 WebView2 在原生画面后加载。`+layout.svelte` 首次完成 hydration 和首帧绘制后调用 `report_startup_ui_ready`；Rust 再显示/聚焦主窗口并隐藏原生画面。开机自启仍默认隐藏，不创建原生画面；若 WebView2 失败，先记录状态，等用户从托盘/快捷键打开窗口后再提供重试与退出选项。
- 监听主导航的 `IsSuccess` 与 WebView2 `ProcessFailed`。启动 45 秒仍未收到界面就绪信号时也视为失败。错误对话框提供重试和退出；重试重新加载主 WebView2。自启失败保存首次失败分类，等用户打开窗口时沿用导航、进程崩溃或超时对应的文案。Tauri 主窗口构建失败时显示可读错误后退出。
- 原生画面无法创建时，主窗口回退到现有 `app.html` loader；超时或 WebView2 失败仍提供原生错误对话框。

## 影响与数据

新增一条无参数的 renderer 生命周期确认命令，以及 Windows 原生窗口和 WebView2 事件监听。错误文案不包含路径或 provider 数据。没有配置、数据库或其他持久数据变化，无需重置或迁移。

## 替代方案

- 仅使用 `app.html` loader：拒绝，因为它在 WebView2 开始加载 HTML 后才可见。
- 以另一个 Tauri WebView 窗口作为 splash：拒绝，因为它增加第二个 WebView2，仍不能覆盖首次 WebView2 初始化，并增加启动资源消耗。
- 原生静态图：拒绝，因为 Haven 已有简洁的三条浮动柱动效，原生窗口可用轻量 GDI 重现该节奏。

## 验证

通过 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、IPC contract 检查（80 handlers）、IPC event 检查（36 channels）、UI check、全量 UI 测试（153 个文件 / 1283 项）、production build 和 ADR 索引检查。Windows 单元回归确认延迟提示保留 WebView2 进程失败文案。仍需在 Windows 桌面运行验收冷启动、导航失败、进程失败、重试、自启隐藏、自启窗口打开后的延迟错误提示，以及原生窗口创建失败。
