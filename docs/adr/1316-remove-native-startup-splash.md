# 1316：移除 Windows 原生启动加载与失败恢复

## 状态

已接受并实现（2026-10-18）。

## 背景

ADR 1154 引入了独立 Win32 加载窗口、启动超时监测和 WebView2 故障恢复。用户要求移除原生 splash，并确认启动失败提示与恢复流程也无需保留。主窗口已有普通启动显示和开机自启隐藏策略；`app.html` 也已有静态加载状态。

## 决定

- 删除独立 Win32 加载窗口、绘制动画、通信通道及其工作线程。普通启动仍在 setup 阶段显示并聚焦 Tauri 主窗口；开机自启继续隐藏主窗口。
- 删除启动协调器、启动 ready command、启动超时监测、WebView2 导航/进程失败监听、重试流程和启动失败对话框。构建错误仍写入日志后退出进程。
- 前端继续在首帧后移除 `app.html` 静态加载状态，不再通知 Rust 启动完成。

## 影响与数据

变化影响启动可视界面、启动失败处理、Tauri IPC command 清单和 app-binary 的 Windows 依赖；不改变持久数据，无需重置或迁移。移除 `webview2-com` 直接依赖及未使用的 `Win32_System_LibraryLoader` feature。WebView2 启动失败时不再提供 Haven 专用重试/退出对话框，builder 错误只记录日志后退出。普通启动直接显示 Tauri 窗口，网页开始绘制后由现有 HTML 加载界面短暂覆盖首帧交接。

## 替代方案

- 保留原生窗口或启动恢复机制：拒绝，因为用户明确要求同时移除启动加载画面和失败恢复 UI。

## 验证

- 通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 `cargo test --workspace --locked`。
- 通过 UI check、全量 UI 测试（161 个文件 / 1337 项）和生产构建。
- 通过 IPC contract 检查（84 handlers）与 IPC event 检查（36 channels）。本轮未执行安装后桌面冷启动的人工视觉验收。
