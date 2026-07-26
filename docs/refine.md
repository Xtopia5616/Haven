# 借鉴 Pi Coding Agent 的可改进项

本文档基于对 Pi Coding Agent 架构的研究，对比当前 Haven 实现，识别可借鉴的改进方向。

## 1. 系统提示词仿写 — 已完成

Pi 的系统提示词极简（<1000 token），风格明快：

```
You are an expert coding assistant operating inside pi, a coding agent harness. ...
Available tools:
${toolsList}
Guidelines:
${guidelines}
```

原 Haven 的 `build_system_prompt` 在提示词中注入了完整 JSON Schema。已按 Pi 风格重写，但保留了内置工具的全量 schema（仅 MCP 和 Skills 精简为 name + description）：
- 内置工具：完整 JSON Schema 注入（与 API 的 `tools` 参数互补）
- MCP 工具：仅 name + description，前缀标注
- Skills：仅 name + description，通过 `load_skill` 激活
- 指示部分精简为 5 条清晰指引
- 用户事实/偏好保持单行紧凑格式

**涉及文件：** `crates/agent/src/lib.rs` — `build_system_prompt()`

## 2. 树状会话历史

Pi 支持分支式会话树，Haven 使用扁平的 canonical message 列表。当 LLM 走错路线时无法"回退到分支点重试"。

**改进建议：**
- 在 `ReActSnapshot` 中保存可回溯的分支点
- 在 UI 提供「回退到上一步」或「查看分支」功能
- 消息存储模型增加 `parent_message_id` 支持树形结构

## 3. 工具调用消息结构 — 已完成

Haven 将工具结果以 `User` 角色的纯文本注入（"Tool 'X' result: Y"），丧失了原生 tool calling API 的结构化关联（`tool_call_id` + `tool` role）。

**改进：**
- 消息结构：`CanonicalMessage` 已存在 `Tool` 角色和 `tool_call_id` 字段，现正确使用
- Agent 层：工具结果使用 `CanonicalRole::Tool` + `tool_call_id`，替代 `CanonicalRole::User` + "Tool 'X' result: Y" 纯文本格式
- Assistant 消息：LLM 响应包含 tool_calls 时，assistant 消息附带 `tool_calls` 字段
- `LlmRole` 新增 `Tool` 变体，`LlmMessage` 新增 `tool_call_id` 字段
- `convert_to_llm` 正确映射 `Tool` 角色并透传 `tool_call_id`
- OpenAI 客户端：`OpenAiMessage` 新增 `tool_call_id` 字段，`convert_messages` 处理 `tool` 角色
- Action 模型：增加 `tool_call_id: Option<String>` 字段，在 `parse_reasoner_response` 中从 LLM response 的 tool_call id 填充

**涉及文件：**
- `crates/agent/src/lib.rs` — `Action` 结构体、`parse_reasoner_response`、`run_task_resumed`、`run_task`
- `crates/llm/src/types.rs` — `LlmRole`、`LlmMessage`、`convert_to_llm`
- `crates/llm/src/client.rs` — `OpenAiMessage`、`convert_messages`

## 4. 扩展 SDK / 插件机制

Pi 的扩展系统允许 TypeScript 模块添加工具、命令、UI 组件。Haven 依赖 MCP（外部进程）和 Skills（沙箱脚本），缺乏轻量级 Rust 级插件机制。

**改进建议：**
- 设计 `Plugin` trait（类似 `Tool` trait 但包含初始化、生命周期钩子）
- 支持通过 `dlopen` / `libloading` 加载动态库注册工具
- 简化 MCP 适配器模板，降低编写自定义工具的入门门槛

## 5. AgentEventEmitter 单体 trait — 已完成

Emitter trait 原包含 12 个方法，每增加一种事件类型都需要修改所有实现。

**改进：**
- 定义 `AgentEvent` 枚举，包含所有 12 种事件变体（Thought、Action、Observation、TaskCreated、TaskCompleted、TaskUpdated、TaskError、FallbackActivated、ThoughtChunk、ReasoningChunk、Supplement、Compaction）
- `AgentEventEmitter` trait 简化为单方法 `async fn emit(&self, event: AgentEvent)`
- `TauriEmitter` 使用 `match event` 处理关心的变体，新增 `Compaction` 事件处理
- `RecordingEmitter`（测试用）使用 `match event` 处理关心的变体，其余用 `_ => {}` 忽略
- 所有 `emit_*` 辅助方法改为构造 `AgentEvent::*` 变体调用 `emit`

**涉及文件：**
- `crates/agent/src/lib.rs` — `AgentEvent` 枚举、`AgentEventEmitter` trait、`RecordingEmitter`、所有 `emit_*` 辅助方法
- `crates/app-binary/src/lib.rs` — `TauriEmitter`

## 6. load_skill 造成共享可变状态 — 已完成

`load_skill` 工具原在运行时修改全局 ToolRegistry。多任务并发调用时存在竞态，且 `rebuild_catalog` 会清除动态注册的技能适配器。

**改进：**
- `LoadSkillTool` 不再写入全局 `ToolRegistry`
- `TaskExecutor::execute_step` 检测 `load_skill` 成功后，自动通过 `ToolsManager::register_for_task` 注册到 per-task overlay
- `execute_tool` 和 `get_risk_level` 优先查找 per-task 注册，找不到才回退全局 registry
- 任务完成/取消/错误时自动清理 per-task 注册

**涉及文件：** `crates/tools/src/lib.rs`、`crates/tools/src/builtin/load_skill.rs`、`crates/tools/src/builtin/mod.rs`、`crates/agent/src/task.rs`

## 7. Token 估算精度 — 已完成

`ContextCompactor` 原使用 `chars/4` 粗略估算 token 数，中英文混合文本偏差较大。

**改进：**
- 引入 `tiktoken-rs`，使用 `o200k_base`（GPT-4o 编码器）精确计数
- 通过 `std::sync::LazyLock` 全局初始化一次 tokenizer，线程安全且零开销

**涉及文件：** `crates/agent/Cargo.toml`、`crates/agent/src/compactor.rs`

## 8. 冗余的 thought 过滤 — 已完成

Haven 在 `parse_reasoner_response` 中过滤 thought 中以 "Action:" 和 "Final Answer:" 开头的行。使用原生 tool_calling API 后已是遗留逻辑。

**改进建议：**
- 移除 `Action:` / `Final Answer:` 前缀过滤逻辑
- 如担心模型错误输出文本格式 tool call，改为正则检测 JSON block 而非行前缀

## 9. 工具并行执行的结果归并 — 已完成

`run_task` 中使用 `join_all` 并行执行同一步骤中的多个工具，但结果依次推入 canonical messages。若某工具执行较慢，整体步骤被拖慢。

**改进建议：**
- 保持并行执行，但结果归并时使用 `select!` / `FuturesUnordered` 按完成顺序逐个推入

## 10. 无假设检验 / 分支尝试机制

Pi 的树状会话允许 agent 尝试多条路径。Haven 的线性 ReAct 只能一条路走到黑。

**改进建议：**
- 在规划阶段显式生成多个备选方案，序列化尝试
- 或轻量实现：当工具返回错误时，直接生成替代方案继续

## 11. 用户名称偏好注入 — 已完成

模型将自身（Haven）与用户混淆。在 Agent 初始化时通过 `db.set_preference("name", "Xtopia")` 写入偏好，自动出现在 system prompt 的 `Preferences:` 段落中，告知模型用户的名字。

**涉及文件：** `crates/agent/src/lib.rs` — `AgentLayer::new()`
