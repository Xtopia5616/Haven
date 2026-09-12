# ADR 0128: Reduce model decision friction in live tool contracts

> 历史说明：独立 `audio` 模型工具已由 ADR 0133 删除；本文的 `audio.record` 均指
> `media(operation="record")`。

## Background

一次真实 Haven 会话暴露了五类摩擦：模型看到的媒体 operation 可能只在配置层存在、长 JSON observation 会把续读游标切掉、仓库任务的相对路径默认落在 Temp、只读失败仍可能显示为未知幂等性，以及 runtime snapshot 没有把这些状态用一份可执行的能力判断表达出来。

这些问题都发生在模型每一步的决策路径上。它们不要求新增顶层工具，也不应通过放宽安全网关解决。

## Decision

1. 在 `haven-tools` 目录重建时，媒体能力使用实际路由角色、角色配置和 provider capability profile 的交集；录音 operation 使用共享录音管线的实际配置状态。`media(operation="record")`、`media(operation="transcribe")` 和富媒体文件转发共享同一专用 STT 客户端，未配置时才使用 LLM STT 路径。schema 过滤和 prompt snapshot 复用同一套 manager/builtin 判定。
2. 将 `ToolResult` 的模型 observation 改为 structured-first：错误、结果元数据和恢复游标优先，正文和大集合后置压缩。纯文本结果维持原有 Unicode-safe 上限行为。
3. 将仓库根发现收口到 `haven-common::discover_workspace_root`。`shell` / `files` 的相对路径在仓库会话中解析到工作区；显式绝对路径、受管媒体和安全授权仍由原边界处理；非仓库会话回退到 Temp。
4. 在工具定义 JSON 中增加 `retry_safety` 静态元数据，并在具体失败 observation 中附带实际 operation 的 `idempotent` / `non_idempotent` / `unknown` 值。executor 仍只自动重试幂等且 transient 的结果，未知超时与副作用操作不自动重放。
5. 对 Windows PowerShell 5.1 的明显 `&&` / `||` 链式语法做执行前校验；schema 顶部说明当前 shell 的硬规则。

## Alternatives

- 立即把所有聚合工具拆成新的顶层 provider 工具：理论上可显著减少 schema，但会同时改变权限矩阵、UI renderer、session catalog 和旧步骤恢复，留给 P1 的单独迁移。
- 只在描述中声明“不可用”：不能阻止模型先调用不可用 operation，因此本 ADR 要求 schema 与 snapshot 同步裁剪。
- 任何失败都允许重试：会把未知终止和副作用重放成数据风险；本 ADR 只补齐已存在的 operation-level idempotency，并把判定呈现给模型。
- 把所有相对路径强制加入 workspace root：会破坏 Temp 沙箱和用户指定的运行目录，因此只在检测到仓库且没有显式路径时采用工作区默认。

## Impact

- `ToolDef.json()` 增加一个可选、向前兼容的 `retry_safety` 字段；provider-facing `ToolDefinition` 不把它转发到 provider 参数 schema。
- `audio`、`media` 和 `files` 的模型可见 operation 可能减少，但不可用能力不再浪费一次真实调用；能力热更新会触发 catalog rebuild。
- 相对路径的默认语义在仓库会话改变为 workspace root；显式 `cwd`、绝对路径和安全边界优先级不变。
- 不新增数据库表、字段或迁移，不改变 X12 snapshot 的权威事件语义。

## Verification

- `haven-common` 测试 workspace root marker 发现和 ToolDef retry 字段。
- `haven-tools` 测试 structured-first observation、失败元数据、files 的读写幂等性和 PowerShell 语法预校验。
- `haven-llm` 保持现有 provider schema projection 测试，确认新增 common metadata 不进入 provider function parameters。
- 完成后运行 workspace Rust tests、clippy 和 UI gates；若桌面开发进程占用 Cargo 锁，记录为环境限制并在锁释放后重跑。

## Rollback / reset

这是代码与模型视图契约的加性变更，回退 0128 不需要数据库或配置重置。回退时必须同时删除 `ToolDef.json()` 的 `retry_safety`、runtime snapshot 的新能力行、workspace 默认路径逻辑和相应正/负测试，避免出现提示与执行语义漂移。
