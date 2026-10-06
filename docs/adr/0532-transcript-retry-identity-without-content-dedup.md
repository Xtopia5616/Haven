# ADR 0532：Transcript 重试按身份关联，不按内容去重

## 状态

已采纳并实施；验证完成（2026-10-06）。

## 背景

三个 UI/Agent 路径曾用文本判断输入身份：Continue 恢复后按最后一条用户消息的内容决定是否重发；新 run 组装 system Additional context 时按文本跳过初始用户行；`submitTranscript` 按相同文本和请求属性合并并发提交。相同文本可以是两条合法输入，内容不能证明请求相同。

## 决定

1. Continue 策略携带原用户消息的 `message_id`。数据库重载后只查找这个确切 ID：存在则依赖 Pending resume；缺失或原 ID 不是 durable `msg-*` 时重发原文本。
2. 初始 Additional context 只按 `initial_message_id` 排除会话首条输入。ID 不可用时保留历史，不按文本猜测。
3. `submitTranscript` 只有在调用者提供相同 `submissionToken` 时才把并发调用合并为同一 promise。没有 token 的输入即使内容相同也按独立消息排队。Continue 重试按原消息 ID 派生 token；录音提交按 `recordingSessionId` 派生 token。
4. Token 仅供进程内协调，不进入数据库或 IPC；不更改 transcript、事件或持久化契约。

## 替代方案

- 按文本、时间邻近或末尾消息推断相同用户意图：拒绝。相同文本可代表不同消息，重载和并发也没有稳定的内容身份。
- 删除 in-flight lane：拒绝。它仍负责同会话的顺序提交；只是合并条件改为显式请求身份。

## 影响与验证

- UI 的重发判断和 prompt history 过滤使用 durable message identity。没有共享 token 的同文本并发输入现在都会排队并写入各自 transcript 行。
- 修改不涉及 schema、Rust→TypeScript IPC payload 或用户数据重置。
- 更新既有 Continue 与 submit 协调用例以反映 ID/token 契约。`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run`、`corepack pnpm --dir ui run build` 和 ADR index 检查均通过。Rust workspace 中标记为手动性能测试的用例按约定忽略；UI 测试 122 个文件、974 个用例通过。

## 回滚

恢复旧的文本比较与并发合并逻辑，并同步回滚本 ADR 和 `AGENTS.md` 中的提交身份规则。无持久化回滚或数据重置。
