# ADR 1213：策略撤销时终止 MCP stdio 进程

## 状态

已实现。MCP stdio client generation 绑定 Tools 当前 process-boundary 快照。原全局撤销范围现由 [ADR 1216](1216-scope-mcp-stdio-process-revocation.md) 收窄：sandbox、writable roots、network 或 execution workspace 变化会断开旧 stdio client；无关的权限模式、trust 和工具设置变化不会断开。

## 背景

[ADR 1211](1211-revoke-running-shells-on-policy-change.md) 与 [ADR 1212](1212-revoke-running-skills-on-policy-change.md) 已为 Shell、Skill 主进程及 Skill setup 进程接入策略撤销和 spawn permit。MCP stdio 仍只在首次连接时检查网络策略：已连接 server 可在网络、sandbox 或 workspace 策略变化后继续运行，health monitor 也可能用旧 client 自动重启 server。

MCP stdio 是本地外部进程，Haven 无法从协议流量证明其实际网络目的地。现有授权契约要求 `NetworkPolicy::Open` 才可启动；这一限制必须覆盖首次启动和同一 client generation 的重连，并在策略变更后回收已运行进程。

## 决定

- `AuthorizationEngine` 仍是策略 revision 与变更 signal 的唯一 owner。Tools 在 MCP 配置连接开始前订阅 signal；每次全局 policy revision 变化都会让 MCP manager 移除所有 stdio client generations。HTTP clients 不因这项进程撤销规则被移除。
- 每个由 manager 创建的 stdio client 固定持有创建时的授权快照。只有当前 `NetworkPolicy::Open` 才能捕获该快照；快照绑定 policy revision、workspace generation 和 signal token，不是调用 MCP tool 的 execution proof。
- stdio 首次启动与 health-monitor/manual reconnect 共用同一授权快照。每次 OS spawn 前都重新取得短期 policy read permit，并一直持有到子进程加入 process containment、初始线程恢复后。策略更新如果先取得写锁，旧快照拒绝 spawn；如果 spawn 已取得读租约，更新等待 attach/resume 完成，再发布撤销 signal 并回收进程。
- 策略变更后的 stdio client 不会自动重建。新的配置发现/refresh 会基于当前策略创建新 generation；网络策略仍需为 `Open`。
- 撤销粒度沿用 ADR 1211 的全局 `policy_revision`：任何会推进 revision 的策略撤销都会终止所有旧快照绑定的 MCP stdio servers。权限 grant 不发布撤销 signal。HTTP MCP 当前是独立网络 transport，不由本 ADR 的 stdio process authorization 控制。
- Windows Job Object / 其他平台 containment 负责回收进程树。本次只使 Haven 授权入口和进程生命周期与策略一致，不构成 OS 文件系统或网络 sandbox；stdio server 仍具有当前 OS 用户所拥有的系统权限。

## 替代方案

- 只在首次 `connect` 检查网络策略：拒绝；旧 client 的健康重连会绕过新策略。
- policy change 后只更新 `network_policy` 字段：拒绝；其他边界变化也推进全局 revision，而且运行中的进程没有被终止。
- 在 MCP server 整个运行期间持有策略读锁：拒绝；持续运行会阻塞安全设置更新和撤销。
- 用 MCP tool confirmation proof 授权 server 子进程：拒绝；启动 server 与调用 server 暴露的单个工具是不同 capability，不应共享调用 proof。

## 影响与迁移

新增进程内授权 port、client generation 快照和 manager policy watcher；没有持久字段、配置字段、IPC 或数据库变化，无需重置或迁移。破坏性内部构造 API 只留给 test/test-support；生产 stdio 必须通过 Tools manager 注入当前授权快照。策略变更会断开已有 stdio server，用户需由显式发现/刷新操作建立新连接。

## 验证

- MCP 负向哨兵验证 `NetworkPolicy::Open` 下缺少 Tools 授权快照仍不能创建子进程。
- Tools 回归验证只允许 `Open` 捕获快照，旧快照在 policy revision 改变后不能取得 spawn permit。
- MCP manager 回归验证 policy signal 会移除 stdio client 并保留 HTTP client。
- 既有 Windows MCP stdio process-tree 测试验证 transport 清理会终止 server 与后代进程。
- Windows 集成回归 `stdio_policy_revocation_terminates_a_live_process_tree` 通过真实 MCP stdio server 和持锁后代进程验证：policy signal 移除 live generation 后，server 与整棵后代树都被终止。
- 本切片运行 MCP/Tools 相关测试、workspace locked tests/clippy、格式检查和 ADR 索引检查；依赖 Windows OS 进程树的最终验收仍以 Windows CI 为准。
- 集成回归补充后，MCP crate 的 99 项测试、该 crate 的 Clippy 和 workspace 格式检查通过。

## 回滚

回滚时移除 MCP stdio authorization port、policy watcher 和 client generation 快照，恢复仅在连接时检查 `NetworkPolicy` 的行为。这会重新允许旧 stdio servers 在策略变更后继续运行或由 health monitor 使用陈旧授权重启。无持久数据需要恢复。
