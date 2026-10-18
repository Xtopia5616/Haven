# ADR 1317：补充轻量启动失败反馈

## 状态

已接受并实现（2026-10-18）。

## 背景

ADR 1316 移除原生 splash、启动协调器和失败恢复流程后，前端若未能完成挂载会一直显示静态加载层；WebView2 导航或渲染进程失败、配置错误、核心状态或 Tauri 主界面构建失败也缺少清楚反馈。用户希望有错误处理，但不恢复启动画面。

## 决定

- 保持普通启动直接显示主窗口，不增加 splash 或启动进度 UI。
- 静态首帧层在 30 秒后仍未被已挂载的 Svelte layout 移除时，显示可访问的启动错误提示和“重新加载”按钮。页面成功挂载时仍由 layout 移除该层。
- 监听 WebView2 主导航失败和渲染进程失败；交互启动显示“重试加载 / 取消并退出”对话框。`--autostart` 隐藏期间将首个失败分类暂存，用户随后打开窗口时再提示。
- 配置、AppState 或 Tauri builder 的 fatal startup failure 先写脱敏日志；交互启动显示一条 Windows 错误提示，附带经过公共清洗器限长和脱敏的原因。若配置文件不可读取，启动日志使用默认的 `%APPDATA%/haven/logs/haven.log` 路径，以便 autostart 失败也可排查。
- `--autostart` 失败不弹模态对话框，避免阻塞后台启动；仍记录错误并以非零状态退出。
- 失败的 AppState 不继续以部分初始化状态运行。用户修复原因后重新启动应用。

## 影响与数据

仅改变首次启动失败时的反馈，不增加 IPC、配置或持久化状态，无需重置数据。前端未完成挂载可整页重载；导航或 WebView2 进程失败可重试加载或退出；后端 fatal failure 退出应用。错误详情经过 `sanitize_error_text` 后才进入用户提示。

## 替代方案

- 恢复 ADR 1154 的完整超时协调器和原生 splash：拒绝，仅保留检测导航/进程失败所需的事件监听。
- 不处理失败：拒绝，会让用户面对永久加载状态或无提示退出。

## 验证

- Rust 启动错误文案、autostart 延迟提示和脱敏规则有单元测试。
- UI 静态 fallback 标记和重试动作有测试。
- 通过 `cargo fmt --all -- --check`、`cargo check --locked -p haven-app-binary`、`cargo clippy --locked -p haven-app-binary -- -D warnings` 和 `cargo test --locked -p haven-app-binary`（260 passed）。
- 通过 UI check、全量 UI 测试（161 个文件 / 1339 项）和生产构建；CSP manifest 包含新增的静态 fallback 脚本。
- 通过 ADR index 检查（1300 条记录）与 `git diff --check`。本轮未执行安装后桌面冷启动及故障注入的人工验收。
