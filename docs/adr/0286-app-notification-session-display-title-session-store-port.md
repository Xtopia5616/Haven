# ADR 0286：App 桌面通知标题通过 SessionStore 同步端口读取

- 状态：Accepted
- 日期：2026-09-24
- 范围：App Windows 桌面通知的会话展示标题 fallback
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0277](0277-context-source-session-title-port.md)、[ADR 0283](0283-app-session-record-lookups-through-session-store.md)、[ADR 0284](0284-end-session-display-title-session-store-port.md)

## 背景

`DesktopNotifications::session_display_title` 在标题缓存 miss 时直接通过
`AppState.db.get_session` 读取会话记录。`ApplicationRuntime` 已注入
`SessionStore`，且同步 `session_record` 已提供完全相同的 session-record lookup
语义。通知由同步事件处理路径调用，不应为这次边界收口改变调用模型。

## 决策

1. 缓存 miss 时通过 `ApplicationRuntime.session_store.session_record` 同步读取；不新增同义 SessionStore port，也不增加异步调度。
2. 保持标题解析顺序：通知标题缓存优先；持久记录的非空 `title` 次之；`title` 为空时使用非空 `input_text`；记录缺失、字段均为空或查询失败时使用 `session_id`。查询失败继续记录脱敏 warning。
3. 将缓存 miss 的解析结果写回既有标题缓存。`SessionCreated` 通知仍只用事件携带的非空 title 或 session id，不把原始输入显示为创建通知标题。
4. 状态事件处理、通知开关、展示文本、同步线程调用模型、IPC 与数据库均不变。

无需新增窄 title port：已有同步 typed record port 已覆盖需要的字段；异步 port 会改变通知处理模型，重复查询接口则会复制已有语义。

## 影响与验证

- `DesktopNotifications` 不再直接访问 `AppState.db` 读取 session。
- 通知模块单测通过 `SessionStore::session_record` 覆盖持久标题、输入文本、空标题、缺失记录与查询失败的回退。
- 无 schema、持久化数据、IPC 或用户可见通知行为变化。

## 回滚

恢复 `DesktopNotifications::session_display_title` 对 `AppState.db.get_session` 的调用并删除对应 ADR 与索引项即可。无数据重置要求。
