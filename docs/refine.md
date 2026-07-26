# 借鉴 Pi Coding Agent 的可改进项

本文档基于对 Pi Coding Agent 架构的全面研究，对比当前 Haven 项目实现，识别可借鉴的改进方向。

> 对照版本：Pi Coding Agent (@mariozechner/pi-coding-agent) | Haven (Rust/Tauri 2 + Svelte 5)

---

## 1. 系统提示词精简 — 已完成

Pi 的系统提示词 <1000 token，工具仅列名称和简短描述。Haven 原注入完整 JSON Schema。

已按 Pi 风格精简，但内置工具仍保留全量 schema（因 API `tools` 参数与提示词互补，两者同时提供可提高模型调用准确率）。MCP/Skills 改为仅 name + description + `load_*` 渐进加载。

- 内置工具：保留完整 JSON schema
- MCP 工具：仅 name + description，前缀标注
- Skills：仅 name + description，通过 `load_skill` 激活
- 指示精简为 5 条

## 2. 核心循环代码膨胀与重复

Pi 的整个 agent loop **仅 418 行 TypeScript**。Haven `crates/agent/src/lib.rs` ~2500 行。

**主要问题：**
- `run_task`（1378-1942 行）与 `run_task_resumed`（828-1375 行）存在 ~900 行重复代码（LLM 调用、流式 chunk 处理、工具执行、状态检查逻辑几乎完全一致）
- 流式 chunk 消费者（`tokio::spawn` 块）在 `run_task` 中定义两次（正常路径 + compaction 重试路径），`run_task_resumed` 同理，共 4 份拷贝
- 提议提取：`async fn run_react_loop()` 作为共享内核，`run_task` 和 `run_task_resumed` 仅做前置状态准备

## 3. 工具定义与 API 参数冗余

Pi 只将工具 schema 通过 API `tools` 参数传递，提示词中仅列名称行。Haven 同时在提示词和 API 中传递完整 JSON schema。

**分析：** 双重传递在模型不支持 `tools` 参数时有价值，但主流模型均已支持，目前策略可以优化。还需关注：
- `build_system_prompt` 每次调用都重建所有工具 schema → 可缓存 schema JSON 字符串，仅工具注册表变化时重建
- 工具描述可进一步精简（去掉 `type`、`required` 等冗余字段）

## 4. 上下文压缩触发策略

Pi 的 `pi-compactor` 在接近 token 限制时压缩。Haven 的 `ContextCompactor` 类似但：
- 仅在 LLM 返回 `ContextLengthExceeded` 错误时触发被动压缩（`run_task` 第 1019-1120 行），而非在调用前主动预防
- `maybe_compact` 虽在每次 LLM 调用前检查，但阈值保守（`context_window - reserve_tokens`）
- 建议：主动压缩阈值应更激进，预留更多 buffer 避免昂贵的重试

## 5. 熔断机制未覆盖工具执行

`LlmRouter` 的 `CircuitBreaker` 仅保护 LLM API 调用。工具执行层（`ToolsManager::execute_tool`）虽支持重试，但**没有熔断**——某个工具持续失败时不会快速拒绝。

**实现建议：** 在 `ToolRegistry` 或 `ToolsManager` 增加 per-tool 熔断器，连续失败 N 次后快速失败，冷却后再试。

## 6. 成本追踪字段存在但未实现

`haven_llm::types::Usage` 包含 `cost: Option<f64>` 字段，但**全程未被赋值**（始终为 None）。Pi 将成本追踪作为核心功能。

**实现建议：**
- 在 `ModelEndpoint` 增加 `cost_per_1k_input_tokens` / `cost_per_1k_output_tokens` 配置
- `LlmRouter` 或 `HttpLlmClient` 在收到 `usage` 后自动计算 cost
- 在 `AgentEvent::Usage`（或新增事件）中推送给前端展示

## 7. 工具并行执行与回退

Pi 的 ReAct loop 是串行的——一步一个工具。Haven 使用 `FuturesUnordered` 并行执行同一步骤中的多个工具。

**已完成改进：** 原使用 `join_all` 等待最慢的工具，改为 `FuturesUnordered` 按完成顺序逐个推入。

**潜在问题：**
- 共享资源的工具并行可能导致竞态（如同时读写同一文件）
- 并行结果按完成顺序推入 canonical 消息，顺序与 LLM 期望的 tool_call_id 顺序可能不一致
- 建议：只有当工具间无依赖关系时才启用并行，或交由 LLM 指定 `parallel` 标志

## 8. 分步回退（Branch & Rollback）未整合到自动恢复

`ReActSnapshot` 支持 `branch_points`，`fork_task` 和 `rollback_task` 已实现，但目前只能通过前端主动触发。

**对比 Pi：** Pi 的树状会话重启后自动从分支点继续。Haven 的自动恢复仅在层面——工具失败时注入"请尝试不同方法"指示（第 1326-1337 行），但不会自动 rollback。

**改进建议：**
- 相同工具在相邻步骤连续失败 N 次后自动回退到上一个 branch point
- 将 `branch_points` 的创建时机改为"LLM 输出多个候选方案时"，而非仅在工具执行前

## 9. AgentLayer 职责过重

Pi 的 `pi-agent-core` 仅关注 ReAct loop。Haven 的 `AgentLayer` 混入了：
- ReAct loop（`run_task` / `run_task_resumed`）
- Session 管理（`ensure_session` / `start_new_session`）
- 事件发射（`emit_*` 系列方法）
- 事实推断（`run_fact_inference`）
- 偏好推断（`run_preference_inference`）
- System prompt 构建

初步拆分建议：
- `SessionManager`（session CRUD + persist_message + conversation_history）
- `InferenceEngine`（fact + preference 推断）
- `SystemPromptBuilder`（prompt 构建 + 工具 schema 缓存）
- `ReActEngine` 专注于 loop 逻辑

## 10. 死代码与未使用功能

crates/agent/src/lib.rs 中存在：
- `#[allow(dead_code)]` 标记的方法：`save_snapshot`（第 768 行）、`rollback_to_step`（第 812 行）
- ReAct loop 的 `outer loop`（第 863/1415 行 `loop { for step_num in ... }`）的 `followups` 处理——`get_followup` 实现在 TaskExecutor 但当前没有生产者调用 `add_followup`

提议：
- 移除未使用的 `rollback_to_step` 静态方法（rollback_task 已用不同方式实现）
- 明确 `followup_queue` 的生产者接入点
- 或移除 followup 机制简化代码

## 11. 测试覆盖率缺口

- `crates/agent` 测试集中在纯逻辑（解析、构造），缺少对 `run_task` 核心循环的集成测试
- `crates/task` 的 dispatcher 测试使用 sleep-based 同步，存在 flakiness 风险
- `FuturesUnordered` 并行执行路径无测试覆盖
- Compaction 重试路径无测试

**参考 Pi：** Pi 提供 `MockLlmClient` 和 `MockToolExecutor` 用于完整的 ReAct 集成测试。

## 12. 前端 event 频率控制

`AgentEvent::ThoughtChunk` 和 `ReasoningChunk` 对每个 token 分别 emit。高频事件通过 Tauri 的 `invoke` 到前端时可能造成性能瓶颈。

**改进建议：**
- 在 chunk 发射端增加 micro-batch（每 50ms 或每 10 个 chunk 聚合一次 emit）
- 或使用 `tokio::sync::watch` channel 替代 unbounded channel，支持背压（现有 unbounded channel 无背压，内存可能暴涨）
