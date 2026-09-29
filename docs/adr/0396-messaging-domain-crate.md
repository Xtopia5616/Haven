# ADR 0396：将消息服务收口到 `haven-messaging`

## 背景

ADR 0069 与 0158 已将跨 session 消息统一到 `MessagingService`，并通过 `SessionMailbox` /
`MessagingRuntime` 接入 `SessionActor`。但 `InboxBus`、服务和这两个协作接口仍由
`haven-tools` 持有；Agent 因而依赖 Tools 才能实现其 session adapter，而 Tools 的 `agent`
内置工具又与通用工具运行时共享一个 crate。

消息领域已有稳定的传输和生命周期契约，适合由独立 crate 拥有。Agent 与 Tools 都通过该
crate 的窄接口协作，不形成 Agent ↔ Tools 的消息层反向依赖。

## 决定

1. 新增 `haven-messaging`，迁入 `InboxBus`、envelope/registry transport 类型、
   `MessageTransport`、`MessagingService`、claim 生命周期及 `SessionMailbox`、
   `MessagingRuntime` 和 peer lifecycle DTO。
2. `haven-agent` 依赖 `haven-messaging`，由 `SessionSupervisor` 实现 `SessionMailbox`，
   并由 `AgentLayer` 实现 `MessagingRuntime`。Agent 的 ReAct、session lifecycle 与 peer
   orchestration 直接使用该领域 API，不再从 `haven-tools` 引入消息类型。
3. `haven-tools` 依赖 `haven-messaging`，继续拥有模型可见的 `agent` 内置工具及其参数、
   operation 与 tool result 映射。它不再定义、保存或重导出消息服务和传输 API。
4. 现有消息服务只由 Tools runtime 创建并在启动 wiring 中绑定 Agent runtime；消息 crate
   不依赖 Tools、Agent 或 App。JSONL transport、同进程优先路由、claim/ack/retry、过期和
   request/reply/receipt 语义均保持不变。

依赖边界：

```text
haven-agent ─┐
             ├──► haven-messaging
haven-tools ─┘
```

## 替代方案

- 继续让 Agent 从 `haven-tools` 引入 `MessagingService` 等消息 API：拒绝，这会把消息服务和
  协作契约继续挂在工具能力 crate 上，并进一步耦合 Agent 与该实现层。
- 将接口放入 `haven-common`：拒绝。消息传输、claim lease、文件总线与 peer lifecycle 是一个
  有行为的领域服务，不是跨域稳定数据类型或纯函数。
- 让 Agent 或 Tools 单独拥有服务：拒绝。前者会把工具能力入口放入会话编排层，后者则保留
  当前依赖方向问题。

## 影响与验证

这是 crate 所有权和依赖图变更，不更改 JSONL 文件、envelope、消息 ID、IPC、数据库或用户配置，
不需要用户数据重置。行为回归继续由迁移后的 `haven-messaging`、`haven-agent` 和 `haven-tools`
测试覆盖。检查 workspace metadata 确认 Agent/Tools 都只向消息 crate 依赖，不引入循环依赖。

验证命令：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚

回退该 crate 拆分提交并将两个模块恢复至 `haven-tools` 即可。文件总线中的 mailbox/archive 数据
保持原样，新旧实现使用相同 JSONL 格式。
