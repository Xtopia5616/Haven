# ADR 0005：为 Tauri WebView 设置明确的内容安全策略

日期：2026-08-26  
状态：已采纳

## 背景

桌面端此前将 Tauri 的 `app.security.csp` 设为 `null`。这意味着渲染进程没有默认资源
限制；即使模型 Markdown 已禁用 HTML，未来的渲染缺陷或受污染的 UI 数据仍可扩大影响面。

## 决定

- 生产构建使用显式 CSP：默认只允许同源资源，IPC 仅允许 Tauri 的 `ipc:` /
  `http://ipc.localhost` 通道；图片和媒体只额外允许 Tauri asset、`data:` 与 `blob:`。
- 禁止对象、frame、表单提交和任意脚本求值；生产脚本不允许 `unsafe-inline`。Tauri 在构建时
  为静态脚本和样式加入 hash/nonce。
- 保留 `style-src 'unsafe-inline'`，因为 Svelte 组件以受控绑定生成动态 `style` 属性；它不放宽
  脚本执行。
- 开发环境使用单独的 `devCsp`，仅放行 Vite 的 `http://localhost:4721` 和该端口的 WebSocket，
  并允许开发热更新所需的内联脚本。此例外不进入生产策略。
- 由 Rust 单元测试解析 `tauri.conf.json`，锁定 IPC、禁止项和开发例外，避免 CSP 被再次设回
  `null` 或无意放宽为通配符。

## 替代方案

继续以 `null` 禁用 CSP 会保留无边界的渲染面；为所有动态样式改写为 class 会显著扩大本次
安全改动，且不能替代脚本、连接与 frame 限制。两者均被拒绝。

## 影响、验证与回滚

生产 UI 不再可直接连接任意网络地址或执行内联脚本；受控外链仍须经 `open_external` 的后端
校验。开发热更新继续可用。验证包括 app-binary 单元测试、前端检查与生产构建。此项不改变
数据库、配置或快照，不需要用户重置；若出现未列入的本地资源，应以最小来源补充策略和测试，
而非恢复 `null`。
