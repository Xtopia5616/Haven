# ADR 0722：Bootstrap status 命令复用状态枚举

## 状态

已采纳并实施。

## 背景

App 内部的 `AppState::bootstrap_status` 已返回 `BootstrapStatus::{Loading, Ready}`；`app:bootstrap` 的 `AppBootstrapEvent.status` 也直接使用该 Serde enum，生成前端 closed type 与值清单 `BootstrapStatus` / `BOOTSTRAP_STATUS_VALUES`。但同一状态的读取命令 `get_bootstrap_status` 调用 `as_str()` 并返回 `String`，使 generated command response 丢失已存在的 `loading | ready` 限制。

UI 只在探测值为 `ready` 时开放启动门闩；其它值均不视作 ready。命令值与事件值来自同一 AppState 状态 owner，无需第二份文本投影。

## 决定

- `get_bootstrap_status` 直接返回 `BootstrapStatus`，并从该 enum 生成命令响应类型；删除只为该命令把 enum 转成自由 `String` 的 `BootstrapStatus::as_str`。
- `app:bootstrap.status` 与命令共享 `BootstrapStatus` 和 `BOOTSTRAP_STATUS_VALUES`；Rust Serde JSON 仍是 `"loading"` / `"ready"`。
- UI readiness helper 的类型谓词引用 generated `BootstrapStatus`，输入继续保持 `unknown`，只有 `ready` 可通过。
- 保留命令名、event channel、时序和用户可见行为；不增加自由字符串 fallback。

## 替代方案

- 让命令继续返回 `String`，并在 UI 手写 status union：拒绝。它会让与事件相同的 Rust 状态再次失去 generated owner，重新引入双份词汇。
- 在 App 增加新的 `BootstrapStatusDto`：拒绝。现有 `BootstrapStatus` 已是 App-owned、SerDe 形状与语义均适合 command/event 的闭合 enum，无需同义 DTO。

## 影响与验证

IPC JSON 值不变；静态 TypeScript response 从 `string` 收窄为 generated `BootstrapStatus`。不涉及持久数据、配置或事件顺序，无需迁移或重置。验证重新生成 IPC contract，并运行 Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC command/event checks、ADR index 与 diff checks。

## 回滚

若回滚，恢复 command 的 `String` response 与 `BootstrapStatus::as_str` 转换，并同步 generated contract/UI typing 和本 ADR 对应文档。数据库、配置和事件 payload 无需回滚或重置。
