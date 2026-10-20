# ADR 1333：Session 级 Markdown 工作文档

- 状态：已接受
- 日期：2026-10-19

## 背景

`session_events` 是会话恢复的 append-only 权威事件流，但 ReAct state 只在进程内存在。Transcript 能记录模型说过什么和调用过什么，却没有一个可替换、可直接展示的当前目标/计划/行动摘要。把这类内容放入跨会话 Memory 会扩大生命周期，也会把临时执行笔记误认为长期事实。

## 决定

- 每个 Session 有一份可选的自由格式 Markdown 工作文档，由 Agent 通过 `session.working_document_read` 和 `session.working_document_replace` 读取、整体替换或清空。
- 每次替换追加 `session_working_document_replaced` domain event；活动事件流中的最新 sequence 是当前值。UI 和模型看到当前内容，不展示历史版本。服务端不提供编辑历史或专门的溯源 UX。
- 文档上限为 32 KiB UTF-8。恢复时从活动 domain events 取最新值，并把最多 4096 个 Unicode 字符的 Markdown 快照作为标注为不可信笔记的 prompt context；需要完整文本时模型可调用读取工具。
- Fork 只复制 transcript 前缀；工作文档属于单个 Session，不随 Fork 继承，新建的 child session 从空工作文档开始。
- 写入经 Agent `SessionSupervisor` 的 SessionStore 完成。UI 更新只由提交后的 committed-event bridge 按 durable sequence 发布。`SessionResumeResponse` 携带当前值、当前值 sequence 与同一事件快照的 append-only sequence 水位；前端可在分支回退后恢复较早的当前值，同时拒绝回退前延迟到达的事件或过期快照。
- 工作文档是用户可见的过程笔记，不是执行记录、完成证明或副作用授权。所有其它工具调用继续经过原有统一授权管线。
- UI 使用现有 Markdown renderer，保持 `html: false`；工作文档中的原始 HTML 不执行。
- 不改数据库 schema，不读写 ReActSnapshot，也不把文档复制到 transcript 或跨 Session Memory。

## 替代方案

- 只使用 transcript：无法稳定表达当前版本，用户需要从历史中猜测状态。
- 把文档放入 `ReActState` 或 checkpoint：与当前事件流恢复权威相冲突。
- 使用固定目标/计划/行动 schema：限制模型组织临时工作上下文的方式。
- 复用长期 Memory：会跨越 Session 边界，并混淆计划与用户事实。

## 影响

增加 Agent domain event 约定、Tools capability port、AgentEvent/Tauri IPC DTO、resume DTO 与聊天面板。每次替换只增加一条事件；没有应用层可编辑的历史视图。32 KiB 上限限制单个 payload，UI 只保留当前文档。事件版本数没有上限或清理策略，因此高频替换会持续增加 append-only 事件流占用；这是当前设计接受的存储特性。

## 验证

覆盖 latest-event replay、clear/replace、尺寸边界、工具端读写、prompt 截断/边界、已提交事件 UI 发布、resume contract 与 Markdown 安全渲染；执行 workspace Rust 和 UI IPC 影响面门禁。

## 回滚与数据重置

无需重置数据库：该功能使用既有 generic domain-event schema。回滚代码后，未知的 `session_working_document_replaced` 事件仍是普通非 transcript domain event，不影响 transcript replay；重新启用时仍可读取最新事件。
