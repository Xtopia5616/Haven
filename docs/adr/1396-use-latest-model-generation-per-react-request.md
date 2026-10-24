# ADR 1396：ReAct 每次模型请求读取当前已发布配置代次

## 状态

已接受并验证 — 2026-10-25。替补 ADR 1385 留下的“整个 turn、会话或后台任务是否固定代次”问题；不改变 ADR 1385 的单次请求/媒体操作快照边界。

## 背景

ADR 1385 建立模型与媒体配置的原子发布点：一次 provider 或媒体操作必须从同一不可变快照读取 router、媒体客户端与配置版本；已开始的操作可继续持有旧快照。它没有规定跨多个请求的 ReAct run、会话或后台工作应固定在哪一代。

用户选择以当前最新配置为准。若把配置代次固定在会话或 run 开始时，配置热切换之后仍会让后续请求继续使用旧 provider、凭据和能力声明。

## 决定

1. ReAct 不在 session、run 或 turn 生命周期中固定模型配置代次。每次新的模型请求开始时，从共享 `ModelMediaRuntime` 捕获当前完整快照。
2. 配置切换不会改变已经开始的 provider 请求；该请求及其内部 transport/recovery 尝试继续使用开始时捕获的 router。切换后开始的下一次模型请求读取新代次。
3. ReAct 工具调用、确认等待、用户输入等待、继续生成和后台工作边界不缓存旧 router。每次后续模型请求都重新读取共享运行时。
4. 标题、记忆推理与嵌入、媒体 provider 操作继续遵守相同规则：各自的一个逻辑操作先捕获单一代次，并在该操作结束前保持一致；另一个后续逻辑操作读取最新代次。
5. 本决定不保证一次 ReAct run 的工具定义或会话历史随 provider 切换重建。已经由当前请求生成的工具调用仍按其原有 catalog snapshot 执行；后续模型请求按现行 catalog 与最新模型代次准备。

## 验证

- `router_reload_reaches_title_inference_prompt_recall_and_index` 覆盖 Agent 长期消费者在热切换后使用新 router，并保留在途调用的旧快照。
- `one_react_run_uses_the_latest_model_generation_for_each_request` 使用 fake A/B clients：A 在 ReAct run 内调用一个发布 B 的工具；该 run 随后的模型请求必须到达 B。
- 运行受影响 crate 的 locked 测试、workspace 门禁，并记录真实 Windows UI 验收是否可执行。

## 影响与回滚

没有持久数据、配置、数据库 schema 或 IPC 变化，无需重置。模型切换只影响其后开始的请求；已开始请求按固定快照完成或失败。若回滚，只需撤回本 ADR 与回归测试，并恢复“跨 run 代次”未决状态；不应改变 ADR 1385 的单操作原子快照。
