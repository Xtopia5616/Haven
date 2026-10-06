# ADR 0601：命名 Agent session creation 结果

## 状态

已采纳并实施。

## 背景

AgentLayer 创建会话时先持久化第一条 ingress 用户消息，再注册/唤醒 session，并返回 `(SessionInfo, first_user_message_id)`。Ingress 的新建和过期 active-session 回退路径都按位置解构；peer-session 创建则忽略消息 ID。两个值有稳定且不同的领域角色，tuple 把关系留给调用方记忆。

## 决定

1. 两个创建入口统一返回 `CreatedSession { session, first_user_message_id }`。
2. Ingress 按字段发布新会话并构造 `ProcessResult`；peer-session 路径继续使用 session 信息设置标题与注册 inbox。
3. 创建、首条用户消息持久化、actor 注册、可选 dispatcher 唤醒的先后顺序保持不变。

## 替代方案

- 仅调整局部解构变量名：拒绝，调用方仍依赖 tuple 顺序。
- 把首条消息 ID 合并到 `SessionInfo`：拒绝，消息身份不是 session 信息字段，会扩大运行时 session projection 的职责。

## 影响与验证

- 仅改变 Agent crate 内部方法的返回类型，不改变 IPC、事件、持久化结构或 session 生命周期语义。
- 命名路线图 §5.7 仍保持 Active；其他 crate、UI、IPC/event payload 与配置/持久名继续审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo test --locked -p haven-agent`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(SessionInfo, String)` 返回与调用点 tuple 解构；无持久化迁移。
