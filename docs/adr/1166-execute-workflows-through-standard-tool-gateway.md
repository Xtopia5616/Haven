# ADR 1166：Workflow 子步骤复用标准工具授权与执行网关

## 状态

已采纳；ADR 1163 / 1165 的第二条实现切片。当前先接入同步 MCP Workflow；嵌套确认的暂停/恢复另作后续切片。

## 背景

ADR 1165 已定义有界 Workflow DAG 和静态验证器。模型需要一个可组合多个 MCP 操作的受限入口，但 Agent 的现有确认 continuation 绑定 provider 原生 tool-call batch，不能安全承载一个未完成的嵌套 Workflow。

## 决定

- 提供单个合成 `workflow_run`。仅当会话原生 MCP surface 上至少有一个 Direct 或已加载 Deferred capability 时加入；其 allowlist 和 schema 来自该会话快照。Hidden、未加载 Deferred、CodeMode-only、禁用工具和 MCP 管理类 meta-tool 不进入该 Workflow surface。
- Workflow 只串行执行静态计划；每个节点重新解析当前 capability 和授权请求，独立通过 AuthorizationEngine 获得 proof，再使用标准 Tools `Model` execution surface。Tools 在 handler 前重新验证 live exposure、schema、target binding、安全配置和 proof；不复用 Workflow 外层 proof 或子节点 proof。
- 子步骤输入只来自 Workflow 定义中的 literal、object/list 和已完成结果 JSON Pointer。内部保留的结构化结果只用于后续映射；返回模型的步骤 observation 经原工具的标准 observation renderer，并受步骤/累积字节上限约束。
- 当前确认生命周期不能暂停嵌套调用并持久恢复局部 DAG。因此遇到 `RequiresConfirmation` 时，在该子工具 handler 前 fail closed、停止后续节点，并将已完成节点 ID 与 observation 标成 partial、not-retryable；不会从外层 Workflow 的批准推导子调用批准。Workflow 本身不写 DB、session event 或 ToolRun。用户可用 Legacy 原生 surface 执行需要确认的单步；支持真正暂停/恢复的嵌套确认需先为 session event 定义独立 continuation 契约。
- 节点按拓扑顺序串行运行；任一节点失败、预算耗尽或取消时停止后续节点。已授权且已完成的外部副作用不回滚；返回结果明确列出完成节点，不自动重试整个 Workflow。Workflow 在当前 turn 内执行，不支持自动恢复、保存或 schedule。
- 单步授权是子调用的唯一权限来源。Workflow allowlist 限定可寻址名称，但不代表权限 grant；每步仍由 Tools 检查 live exposure、target、sandbox 与 network policy。

## 替代方案

- 让 workflow 工具持有 MCP manager 或 raw adapter：拒绝；它将绕过 Tools owner 和实时目标验证。
- 用外层 `workflow_run` receipt 批准所有子节点：拒绝；会把单次授权扩大到不同 capability/input/target。
- 确认时先执行节点、之后再暂停：拒绝；确认拒绝或崩溃时可能产生未经批准的副作用。
- 把当前 DAG 序列化为 ToolRun 或 QuickJS continuation：拒绝；这会增加独立持久 owner，且 Workflow 不是真正脱离会话的后台工作。

## 影响与迁移

新增一个当前 turn 内合成模型工具和 Agent→Tools 子调用 port；不新增用户配置、数据库 schema、IPC 或 ToolRun 类型，无需数据重置。Workflow 输入定义严格按 ADR 1165 版本解析，不提供向下兼容层。

## 验证

验证正向串行映射、每步独立授权和 live target/exposure 重检；负向验证 allowlist 越界、确认/拒绝、子工具失败、预算和取消均在后续 handler 前收束，且部分完成结果禁止自动重放。
