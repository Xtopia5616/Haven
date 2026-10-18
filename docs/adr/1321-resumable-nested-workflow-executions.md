# ADR 1321：嵌套确认续接与持久 Workflow ToolRun

## 状态

已采纳；按下列切片实施。

## 背景

ADR 1163 与 1166 将需要确认的 CodeMode / Workflow 子调用 fail closed，因为当前 session confirmation 只续接 provider 原生 tool-call batch。Workflow 也只在当前 turn 内执行。现有 ToolRun 可持久化单工具调用，但没有静态 DAG 的逐节点检查点、恢复与续跑语义。

本变更扩展执行能力，不引入可命名、可编辑或可复用的 Workflow 定义库。

## 决定

### 嵌套确认

1. CodeMode 和 Workflow 的子调用仍独立解析实时策略并取得独立授权 proof。子调用需要确认时，Agent 将其作为原 session 的确认请求暂停当前工具批次；不执行该 handler。
2. 确认请求持久关联父 tool-call identity、子调用 identity、规范参数和原始确认 receipt。批准后，父 CodeMode 脚本或 Workflow DAG 从入口重新驱动，但已完成子调用从 durable checkpoint 回放其原结果，不重新执行 handler。
3. 恢复时对新子调用和待批准子调用重新校验当前 exposure、授权、target、sandbox 与 network policy。原 receipt 仅对其绑定的能力、参数、目标、策略 revision 和有效期有效。
4. 对每个子调用，先记录开始事件，handler 成功后再追加完成事件。恢复时若发现开始事件没有终态完成事件，将该子调用标为结果未知并 fail closed；不得自动重放该节点或任何后续节点。用户可先核实副作用，再显式重新发起操作。
5. CodeMode 恢复时按子调用序号重放完成结果，并逐字比较能力与参数；脚本若无法重现已记录的调用前缀则停止，避免将确认或结果错配给另一调用。QuickJS heap 不持久化；任意 CodeMode 脚本仍不得作为后台或定时任务保存。

### Workflow 持久运行实例

1. Workflow 仍是 ADR 1165 定义的版本化有界静态 DAG。定义只随一次运行实例持久保存；不增加用户级 Workflow 库。
2. 跨 turn、后台或定时的 Workflow 以 ToolRun 为唯一运行实例和生命周期 owner。运行定义、session 关联、触发时间、取消、确认与终态结果由 ToolRun 接口管理；逐子调用的开始/完成记录写入该 session 的 append-only event stream，并以 `tool_run_id` 关联运行。不开新的 Workflow 库或通用 Job executor。
3. 调度触发后每个节点仍经过同一 Tools gateway 和独立实时授权。session 恢复时由 ToolRun 使用相同 `tool_run_id` 重新入队，并从已提交的子调用完成事件恢复输出；已完成 handler 不重放。若进程关闭时某个 handler 已启动但没有完成事件，ToolRun 会再次入队，journal 将该节点判为结果未知并终止自动续跑。
4. Workflow 的授权拒绝、用户拒绝、超时、撤权和取消均停止后续节点；部分副作用不回滚。只有 durable completed checkpoint 证明已完成的节点结果可用于下游 JSON Pointer 映射。
5. 单次 Workflow 定义、节点数、输入/输出、观察文本与每次执行窗口均有进程硬上限；ToolRun admission 还受现有待执行数量与调度期限限制，确认复用既有请求有效期。它们不构成跨确认暂停的累计运行期限或持久历史总量配额。取消会传递到当前子调用，并阻止新节点启动。
6. Inline Workflow 继续使用当前 turn 内的同步入口；需要确认时使用 session confirmation continuation。后台和定时入口创建 ToolRun 并立即返回 `toolrun-*`，终态结果通过既有 ToolRun completion/outbox 交付。

## 替代方案

- 让 QuickJS continuation 或其 heap 成为 durable 状态：拒绝；脚本以有序子调用日志确定性回放，heap 不落库。
- 崩溃后从失败节点自动重试：拒绝；外部 MCP 副作用没有通用幂等键，handler 执行中的结果无法证明未发生。
- 把 Workflow 定义保存到配置或独立用户库：拒绝；本次只持久化运行实例，减少与权限、MCP exposure 变更和编辑生命周期的耦合。
- 新建平行 Job executor：拒绝；后台和定时任务复用 ToolRun、确认 owner、取消与 completion outbox。
- 复用 CodeMode 执行持久 Workflow：拒绝；Workflow 继续使用静态 DAG 及显式数据引用。

## 影响与重置

实现复用 ToolRun 的 `tool_name`/`tool_args` 保存一次运行定义，并用 typed `nested_tool_call_checkpoint` session events 保存子调用状态；不新增数据库表、字段或配置。Workflow continuation DTO 增加嵌套确认数据，旧请求缺少该字段时按普通确认解析。Session event transcript 权威和现有 ToolRun completion/outbox 保持唯一 owner。

## 验收

- CodeMode 与 Workflow 子调用确认在 handler 前暂停；批准后只运行被确认的子调用及剩余调用，拒绝/超时后停止。
- 进程重启后重放已完成子调用结果，不重放 handler；未完成的执行中节点标记未知并 fail closed。
- 子调用参数、目标或 exposure 改变时拒绝续接；receipt 不跨子调用复用。
- 持久 Workflow 可通过 ToolRun 立即后台运行、按时间触发、取消、等待确认及终态恢复；session / MCP 工具被删除、结束或撤权时安全终止。
- 覆盖事件提交失败、SQLite 满、取消竞态、确认超时、并发预算、载荷/结果边界和 ToolRun outbox 重投。
- 按 schema、session event、ToolRun 生命周期与 Rust↔UI 契约影响面执行完整门禁，并验证 Windows 实际确认、重启和恢复流程。
