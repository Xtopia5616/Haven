# ADR 1216：收窄 MCP stdio 进程的撤销范围

## 状态

已实现。MCP stdio client generation 仍在执行边界变化时停止；无关的权限模式、信任和工具设置变化不再断开所有 server。

## 背景

[ADR 1213](1213-revoke-mcp-stdio-on-policy-change.md) 让 MCP stdio manager 订阅全局 `policy_revision`。这能确保网络、sandbox 或执行 workspace 收紧后，旧 server 不能继续运行或用陈旧快照重连，但也会因为 permission mode、session trust revoke、tool settings 等不改变该 server 进程能力的修改而断开所有 stdio servers。

MCP stdio 进程拥有当前 Windows 用户的进程权限，不能在 Haven 的工具层限制任意网络/文件访问；因此网络、sandbox、writable roots 与 execution workspace 边界变化仍必须使旧 generation 失效。单个 MCP 工具调用则由 Tools 在每次执行时重检实时 exposure 和当前授权，可以独立于连接生命周期收紧。

## 决定

- 保留全局 `policy_revision` 与 signal，继续用于 execution proof、Shell 和 Skill 的撤销语义。
- 为 MCP stdio process start 增加单独的 `process_boundary_revision` 与 signal。它只在 sandbox mode、writable roots、network policy 或 execution workspace generation 实际变化时推进。
- MCP manager 只订阅 process-boundary signal。边界变化后仍移除所有旧 stdio client generations；HTTP transport 不受影响，旧 generation 不能自动重连。
- PermissionMode、permanent/session trust 的普通变更、工具开关与其它 `tool_settings` 更新不发送 MCP process-boundary signal。它们仍更新全局授权版本；后续 MCP tool call 必须经 live exposure 和授权判断。
- `McpStdioProcessStartAuthorization` 同时校验独立 boundary revision 与 signal token，并继续校验 workspace generation 与 `NetworkPolicy::Open`。启动 permit 的读锁和原有 spawn 线性化不变。
- 配置应用只在上述边界确有差异时发送 MCP signal；对同一边界的重复设置不得断开 MCP server。安全设置重载若改变边界则仍撤销。

## 替代方案

- 继续让 MCP manager 订阅全局 policy signal：拒绝；与 stdio 进程权限无关的 trust 或 tool setting 更新会造成全局断连。
- 边界改变后只让未来 spawn 失败，保留当前 server：拒绝；已经运行的 opaque process 不会因 spawn 检查而停止。
- 为每个 MCP server 建立配置 watcher 并自行推断是否要断开：拒绝；ExecutionWorkspace、sandbox 与 network policy 的 authority 仍应由 AuthorizationEngine 单独拥有。

## 影响与迁移

变化限于 Tools 进程内授权快照、signal 和 startup composition wiring；没有持久化、配置、IPC 或数据库字段变化，无需迁移/重置。边界变更仍可能断开所有 stdio servers；不相关的 permission mode、trust 或工具设置变化不再触发连接重建。Shell 与 Skill 继续使用 ADR 1211/1212 记录的全局撤销语义。

## 验证

- 新增回归先验证旧实现会因 permission mode 变化拒绝同一 MCP process snapshot。
- Tools 回归验证 permission mode、`tool_settings`、session grant revoke 和不改变边界的 `apply_security` 不更新 MCP process signal，且旧 stdio spawn authorization 仍有效。
- 同一回归验证 network policy 变化会通知 MCP process owner 并拒绝旧快照；workspace 切换也会使旧快照失效。
- `haven-tools` locked 全量测试、严格 Clippy、格式检查及 ADR 索引检查覆盖本切片；MCP manager 进程树回归仍验证断开时回收 server 与后代。

## 回滚

恢复 MCP manager 对全局 `policy_changes` signal 的订阅，删除 process-boundary revision/signal 和新增测试，并撤回本文/索引/路线图更新。无持久数据需要恢复；回滚会恢复无关权限或设置更新也能断开所有 stdio servers 的旧行为。
