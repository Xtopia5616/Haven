# ADR 1326：将管理页 Skill 预览作为会话步骤运行

## 状态

已采纳并实现。真实 HCS guest 与目标 Windows 支持矩阵仍是独立发布 Gate。

## 背景

ADR 1268 为模型发起的 Skill 调用接入了实时授权、可信 session/step 身份、HCS broker 与隔离输出审阅。管理页“运行”按钮仍走不含 session/step 身份的 AppCommand 路径，因此 WorkspaceWrite 下会按设计拒绝；同时，管理页没有 step id 可用于读取现有输出审阅面板。

## 决定

- 管理页把当前选中的会话 id 作为请求上下文传给后端。后端只接受活动、静止、没有待处理输入或交互的 Paused 会话；运行中、Pending、终态或等待交互的会话拒绝此操作。
- Agent 在会话 run admission 下临时运行该会话，后端生成 canonical `step-*` 与 `call-*` 身份，并将 ToolCall 与 ToolResult 作为 `session_events` 的 transcript 事件提交。ToolCall 事件先原子投影待执行步骤，再调用普通 Agent/Tools 授权和执行路径；工具上下文由后端注入 session/step 身份。结束后会话回到 Paused；执行期间排队的新输入保留为 Pending，由正常 dispatcher 接管。
- 管理页使用返回的 `step_id` 显示现有隔离输出审阅面板。Manifest、丢弃和晋升仍由 ADR 1268 的 owner/IPC/Files 授权链路管理。renderer 只选择 session id，不提交 step id。
- 未选择活动会话时，旧 AppCommand 预览路径仍可服务不需要 session/workspace 的 FullAccess 执行；WorkspaceWrite 继续 fail closed，因为这条路径没有可信会话步骤身份。
- 不新增持久化表或 schema 迁移。Managed preview 的 ToolCall/ToolResult 是普通 append-only transcript event，步骤身份复用已有投影契约；后续恢复与 rollback 仍以 `session_events` 为权威。

## 拒绝的方案

- 使用 renderer 生成的 session/step id：允许绑定到另一会话或步骤，拒绝。
- 建立与对话分离的管理运行 id：HCS review IPC 要验证真实 session step，另建身份会造成平行审阅所有权，拒绝。
- 在 WorkspaceWrite 下继续不带身份地调用通用 SkillExecutionPort：不能提供可信步骤和审阅生命周期，继续 fail closed。
- 在 Agent 正运行、会话 Pending 或等待交互时并发执行预览：可能与 ReAct 工具步骤争用步骤序号或执行权，拒绝。

## 影响、回滚与重置

这是 Rust Agent/App 与 UI IPC 的跨层改动，但不改变 SQLite schema 或已存记录解码格式；不需要重置持久数据。管理页预览进入会话 canonical transcript，后续模型上下文会按普通 ToolCall/ToolResult 重放。回滚时删除该入口的 managed-step 方法与 UI review 生命周期，恢复 WorkspaceWrite 管理页 fail-closed；保留 ADR 1268 的模型 Skill HCS 路径。

## 验证

- 必须覆盖静止 Paused 会话准入，以及 Running/Pending/交互等待会话拒绝；验证每次 managed preview 写入一对 transcript 事件并物化真实步骤。
- 必须覆盖 Tauri 请求/响应生成契约、UI 会话身份转发与 review panel 绑定返回的 `step_id`。
- 跨 Rust/UI/IPC 门禁按 `docs/development-standards.md` 执行；真实 HCS guest 与 Windows 支持矩阵未在普通开发环境验证。
