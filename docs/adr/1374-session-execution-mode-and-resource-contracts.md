# ADR 1374：会话级执行方式与 typed 资源合同

## 状态

已实施 — 2026-10-23。完成执行上下文 Slice 2 的核心垂直链和 Slice 5 的会话执行方式入口；更完整的资源类型、MCP exposure 拆分、进程路径盘点和具体边界扩展仍在路线图中跟踪。

## 背景

ADR 1367 将“任务只分析”与“操作是否还需审批”分开，并要求授权根据工具声明的资源工作。此前 `PermissionMode` 把 Plan、自动编辑和无人值守混在一个全局枚举，`NetworkPolicy::Ask` 又同时表达网络限制和询问行为。文件沙箱则通过扫描通用 JSON 中一组字段名猜本地路径，无法区分复制操作的只读源和可写目标。

## 决定

1. 用 `ExecutionMode` 表达三个用户预设：`Analyze`、`Collaborative`、`Unattended`。它分别绑定任务意图和审批策略；Analyze 在授权规则之前拒绝非只读操作，Unattended 在仍需确认时返回拒绝，不会自动放行。`SecurityConfig.execution_mode` 只作为新会话默认值；会话首次装载时将默认值固化为 `session_context.execution_mode_bound` 事件，此后会话可独立修改并跨重启恢复。新建子会话继承父会话执行方式。
2. 会话执行方式通过 `get_session_execution_mode` / `set_session_execution_mode` 命令读取和修改。活动会话执行中拒绝变更；写事件失败时恢复原内存值。会话事件仍是唯一持久恢复来源，没有增加数据库列或 snapshot。
3. `NetworkPolicy` 只保留 `Deny`、`Restricted` 和 `Open`，默认 `Restricted`。`Ask` 不再是资源策略：旧配置字段 `permission_mode` 及旧值通过明确兼容映射进入新预设；旧网络值 `ask` 保守映射为 `restricted`。序列化只写新字段和值。
4. 每个工具可通过 typed `authorization_resources` 声明此次调用的 `LocalPath`（读/写）或 `NetworkDestination`（scheme/host/port）。Files 依据已解析的 operation 声明资源，copy/move 区分源与目标；Shell 只声明显式 cwd；HTTP 声明 endpoint origin。未知工具没有可信资源声明，授权 owner 不再从通用 JSON 字段名推测路径。
5. 授权请求 hash、确认收据和执行 proof 都绑定资源声明。Files 写目标受 `writable_roots` 与工具 `allowed_paths` 约束；读源只接受适用的读取范围，不再因写入操作而被误判为写入目标。HTTP 的完整 URL 仍由原输入 hash 绑定；SSRf、DNS pin、重定向及响应边界继续由 HTTP owner 执行。
6. 标题区分别提供会话工作目录和会话执行方式；Settings 的执行方式选择只设新会话默认值，旧 sandbox/network 细节收进高级资源边界区域。

## 配置与数据影响

现有配置可继续读取：`permission_mode=plan/default/auto_edit/autonomous` 分别映射为 `analyze/collaborative/collaborative/unattended`；新字段使用 `execution_mode=analyze/collaborative/unattended`。旧 `network_policy=ask` 映射为 `restricted`。后续保存会写新字段。已有旧 `auto_edit` 行为会保守收敛到协作确认，不再隐式自动批准编辑。

会话执行方式使用现有 append-only `session_events`，schema 未变；无需重置数据库。配置迁移不删除权限规则，现有 deny/allow 仍由原 owner 和授权顺序处理。

## 替代方案

- 只改设置文案无法改变全局模式对现有会话的作用范围，也无法解决 proof 中缺少明确资源合同的问题。
- 将目录、网络、审批和工具通道继续压成一个等级，会把不同执行能力错误地当成等价权限。
- 在授权 owner 中保留 JSON 字段名扫描，会继续让参数结构而非可信工具合同决定安全语义。

## 验证与回滚

Rust 单元和集成测试覆盖新旧配置值、会话事件恢复、模式继承、授权 proof、Files 源/目标和 HTTP origin 声明；Tauri IPC 由生成器维护 TypeScript 契约。完整门禁和未执行项记录在实施结果中。

回滚代码后，旧二进制无法读取新写入的 `execution_mode` 配置字段；应从升级前的 `config.toml` 备份恢复旧 `permission_mode` 字段，或手工将新字段改回旧字段和值。旧版本会按自身默认策略处理其不认识的会话执行方式事件；数据库 schema 未变化，不需重置数据库。

本 ADR 不完成 ADR 1367 的全部目标。Typed 远程对象/资源范围、MCP exposure 维度拆分、其它子进程隔离能力盘点、边界扩展交互及 ToolRun/Schedule 独立绑定仍属后续切片。
