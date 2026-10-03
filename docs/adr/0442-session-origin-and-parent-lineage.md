# ADR 0442：Session 来源与 peer parent lineage

## 状态

已接受（2026-10-03）。

## 背景

用户创建的对话和 `agent.spawn` 创建的 peer 都写入同一 `sessions` 表，并由
`SessionInfo`、`SessionActor`、dispatcher 与 ReAct run 执行。peer 的 delegated kickoff
仍是首条 user-turn transcript，额外标注为 `peer_kickoff` 并按低信任输入处理。

peer 的父子关系此前只存在 `haven-messaging` 的 inbox/profile registry；SessionStore
读取会话时无法判断创建来源或按 parent 查询 peer。该 registry 同时承载角色、能力、
mailbox 和在线状态，属于协作运行时状态，不能替代会话的持久来源记录。

## 决定

1. `haven-memory::Session` 持有 typed `SessionOrigin`：普通创建为 `User`，`agent.spawn`
   创建为 `AgentSpawn { parent_session_id }`。SessionStore 为两个来源提供同一创建边界；
   peer 继续使用相同的首消息持久化、actor 安装、dispatcher 和 ReAct 流程。
2. `sessions.origin` 与 `sessions.parent_session_id` 是唯一持久来源事实，并建立 parent 查询索引。
   创建 AgentSpawn 行时，SessionStore 在写事务中验证 parent 当前存在；parent id 作为历史
   provenance 不设外键。删除或保留期清除 parent 不会级联删除 child，也不会擦除 child 的
   来源记录。
3. SessionStore 提供按 parent 分页读取直接 peer children 的 typed 查询。Messaging registry
   继续拥有协作角色、能力、消息投递、在线状态及运行时控制；本决定不改变其存储、协议或
   `agent.*` contract。
4. peer session 不转为 Action/后台任务，不增加第二套 Session/Actor/ReAct 类型。
   `SessionInfo`、Agent event/Tauri wire DTO、transcript 与消息事件均不增加来源字段。

## 未采用的方案

- **只依赖 Messaging registry**：来源会随 registry 生命周期与持久会话历史分离，无法从
  SessionStore 按 parent 查询，也不能稳定审计 session 创建路径。
- **用 Action 表示 peer**：会错误地套用任务终态、outbox 与 Action IPC 语义，且拆开现有
  SessionActor/ReAct 会话模型。
- **把来源作为 SessionInfo/UI wire 字段**：当前需求是后端可查询的持久 lineage；将字段暴露
  到 UI 会扩大 IPC 契约，却没有对应产品用途。
- **让 Messaging registry 继续作为 parent 的唯一权威**：它仍是运行中协作控制面的权威，
  但不承担 session 创建来源的耐久历史职责。

## 影响、验证与回滚

数据库 schema 升至 v34；本版本不提供旧 schema 运行时迁移。升级时按
`docs/release-and-reset.md` 删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`。新建普通
Session 默认来源为 `user`；`agent.spawn` 记录为 `agent_spawn` 并保存 parent id。没有
IPC、messaging wire、消息协议、Action 或 ReAct 语义变化。

验证覆盖普通来源默认值、peer 来源写入和 parent 查询、缺失 parent 拒绝，以及删除 parent
后保留 peer lineage。执行 Memory/Agent 定向测试、应用编译和仓库要求的 Rust/UI 门禁。
回滚代码需同时回滚 schema 契约并按重置说明重建数据库；不要让旧版二进制打开 v34 数据库。
