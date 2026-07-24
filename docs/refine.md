# Haven — 参照 Pi Coding Agent 框架的改进项

> 版本: v2.2 | 日期: 2026-07-23 | 基于 Pi Coding Agent (earendil-works/pi-mono) + Oh My Pi (can1357/oh-my-pi) 框架对照分析 + 深度代码审计
> 
> **[2026-07-23] §2 LLM 与 Provider 层 — 16/16 项全部完成 ✓**
> **[2026-07-23] §3 上下文与记忆管理 — 7/7 项全部完成。**
> **[2026-07-22] §4 工具系统与 MCP 所有 8 项改进已完成实现。**
> **[2026-07-22] §1 架构与核心循环 — 6/6 项已完成实现。**

---

## 目录

    1. [架构与核心循环](#1-架构与核心循环)
    2. [LLM 与 Provider 层](#2-llm-与-provider-层)
    3. [上下文与记忆管理](#3-上下文与记忆管理)
    4. [工具系统与 MCP](#4-工具系统与-mcp) 
    5. [错误恢复与可靠性](#5-错误恢复与可靠性)
    6. [配置与可扩展性](#6-配置与可扩展性)
    7. [UI 事件桥接](#7-ui-事件桥接)
    8. [测试覆盖](#8-测试覆盖)
    9. [安全加固](#9-安全加固)
    10. [实施路线图](#10-实施路线图)

---

## 1. 架构与核心循环

### 1.1 Provider-Neutral 消息格式

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **高** — 基础架构级 |
| 状态 | ✅ 已完成 |
| Pi 做了什么 | 定义 `Message { role: user | assistant | toolResult }` 中间格式；每个 Provider 实现 `convertToLlm()` 转换为原生格式；切换 Provider 不影响 Agent 层 |
| Haven 现状 | 消息格式直接使用 OpenAI 的 `LlmMessage { role: String, content: String }`，耦合在 `agent/src/lib.rs:506-514` |
| 改造内容 | 1. `haven-common` 新增 `CanonicalMessage { role: CanonicalRole, content: ContentPart, tool_calls, tool_call_id }`<br>2. `haven-llm` 新增 `convert_to_llm()` 转换函数<br>3. Agent 内部改用 `Vec<CanonicalMessage>`，仅在 LLM 调用边界转换 |
| 涉及文件 | `crates/common/src/types.rs`, `crates/llm/src/types.rs`, `crates/agent/src/lib.rs` |

### 1.2 双层 Agent 循环（Steering vs Follow-up）

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| Pi 做了什么 | 内层循环处理 Thought→Action→Observation（连续工具调用）；外层循环监听 steering_queue / followup_queue（用户追加指令） |
| Haven 现状 | 单层 `for step in 1..=max_steps` 循环（`agent/src/lib.rs:520`）；supplement_queue 是 flat 的消息队列，不区分 steering 和 follow-up 语义 |
| 改造内容 | 拆分为双层：内层处理工具调用循环直到 FinalAnswer；外层读取 steering_queue（中断当前工具序列）/ followup_queue（完成后追加新任务） |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/task/src/lib.rs` |

### 1.3 Agent 状态持久化（暂停恢复上下文恢复）

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** — 功能闭环缺失 |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/agent/src/lib.rs:501` |
| 问题 | `let mut history: Vec<ReActStep> = Vec::new();` — 暂停恢复后永远从空历史开始，丢失所有 ReAct 步骤上下文 |
| 改造内容 | 1. `resume_task()` 从 DB 加载已有 `ReActStep` 历史<br>2. `run_task_from_id()` 检测 Resumed 状态时，调用 `db.task_steps` 重建 `history` 和 `messages` 向量<br>3. `pause_task()` 前将当前 `messages` + `history` 全量写入 DB |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/task/src/lib.rs`, `crates/memory/src/repositories/task_steps.rs` |

### 1.4 多工具并行执行

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/agent/src/lib.rs:460` |
| 问题 | `let tc = &response.tool_calls[0];` — 只处理第一个 tool_call，其余丢弃 |
| 改造内容 | 1. `parse_reasoner_response()` 改为返回 `Vec<Action>`<br>2. 无依赖的 tool_calls 用 `tokio::join!` 并行执行<br>3. 有依赖的按顺序执行 |
| 涉及文件 | `crates/agent/src/lib.rs:447-489, 629-737`, `crates/task/src/lib.rs:394-447` |

### 1.5 LLM 输出结构化解析（finish_reason 枚举化）

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/types.rs:37` |
| 问题 | `finish_reason: Option<String>` — 非结构化字符串；调用方需做魔法字符串比较（`"tool_calls"`, `"stop"`, `"length"`） |
| 改造内容 | 改为 `FinishReason` 枚举：`Stop`, `Length`, `ToolCalls`, `ContentFilter`, `FunctionCall` |
| 涉及文件 | `crates/llm/src/types.rs`, `crates/agent/src/lib.rs:479` |

### 1.6 多模态内容支持

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/types.rs:8` |
| 问题 | `LlmMessage.content: String` — 仅支持纯文本；阻断图片输入（截图工具、视觉识别） |
| 改造内容 | 改为 `ContentPart` 枚举：`Text(String)`, `Image { media_type: String, data: String }` |
| 涉及文件 | `crates/llm/src/types.rs`, `crates/llm/src/client.rs:72-75` |

---

## 2. LLM 与 Provider 层

### 2.1 流截断检测

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:335-382`, `crates/llm/src/router.rs:208-256` |
| 问题 | SSE 流处理无完整性验证。TCP 中途断开时，`finish_reason` 未设置但流被当作正常完成返回；`aggregate_stream_cancellable` 无 guard 检查 |
| 改造内容 | 1. 在流结束后验证 `finish_reason.is_some()`<br>2. 缺失时返回 `LlmError::StreamTruncated`<br>3. Pi 风格：自动重试截断流 |
| 涉及文件 | `crates/llm/src/client.rs`, `crates/llm/src/router.rs`, `crates/llm/src/types.rs` |

### 2.2 结构化错误类型

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:255-259, 325-329`, `crates/llm/src/types.rs:56-81` |
| 问题 | HTTP 错误全被折叠为 `LlmError::RequestFailed`；未区分 429/401/403/5xx 等不同语义。`LlmError` 缺少 `RetryAfter(Duration)`, `ContentFilter`, `ContextLengthExceeded`, `Billing`, `StreamTruncated` 变体 |
| 改造内容 | 1. HTTP 状态码映射到对应 `LlmError` 变体<br>2. 提取 `Retry-After` header 存为 `Duration`<br>3. 新增 `ContextLengthExceeded` 用于触发 auto-compaction |
| 涉及文件 | `crates/llm/src/client.rs`, `crates/llm/src/types.rs`, `crates/llm/src/router.rs` |

### 2.3 Rate Limit 处理（Retry-After 头解析）

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:255-259`, `crates/llm/src/client.rs:438-463` |
| 问题 | 收到 429 时不解析 `Retry-After` header；`with_retry` 使用固定指数退避（`2^attempt` 秒），忽略服务端限流建议 |
| 改造内容 | 1. 解析 `Retry-After`（秒数或 HTTP-date）<br>2. 退避算法取 `max(fixed_backoff, Retry-After)`<br>3. 指数退避 + jitter 参数化（`base=2s, factor=2, max=30s, jitter=0.2`） |
| 涉及文件 | `crates/llm/src/client.rs`, `crates/llm/src/types.rs` |

### 2.4 流式 Tool Call Delta 丢失

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:332-333, 358-359, 362` |
| 问题 | `tool_calls_acc` 向量被填充但从不返回/发送；每个 `StreamChunk` 的 `tool_calls` 被设为 `Vec::new()`，增量 delta 丢失 |
| 改造内容 | 1. 将累积的 `tool_calls_acc` 写入最终 chunk 或在流结束时返回<br>2. 每个 delta chunk 携带增量 tool_call 信息 |
| 涉及文件 | `crates/llm/src/client.rs:335-383` |

### 2.5 Proxy 支持

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:153-160`, `crates/common/src/config.rs:36-58` |
| 问题 | `reqwest::Client::builder()` 无 `proxy()` 配置；全项目 zero proxy 引用 |
| 改造内容 | 1. `ModelEndpoint` 新增 `proxy_url: Option<String>`<br>2. `HttpLlmClient::new` 注入 `reqwest::Proxy`<br>3. 支持 `NO_PROXY` 环境变量 |
| 涉及文件 | `crates/llm/src/client.rs`, `crates/common/src/config.rs` |

### 2.6 Circuit Breaker（熔断器）

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/router.rs:59-65` |
| 问题 | `select_endpoint` 直接路由到可能有故障的端点，无熔断逻辑。连续失败的端点应暂时禁用 |
| 改造内容 | 1. 每个 Endpoint 跟踪最近 N 次调用的失败率<br>2. 失败率 > 50% 且 ≥3 次连续失败时标记为 `Open`<br>3. 30s 后自动 `HalfOpen` 探测一个请求 |
| 涉及文件 | `crates/llm/src/router.rs`（新增 `CircuitBreaker` 结构） |

### 2.7 Multi-Provider 模型自动发现

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| Pi 做了什么 | 内置 catalog + 动态 Provider API 拉取 + `models.json` 自定义 + Extension 注册；Auth 与 Model 分离 |
| Haven 现状 | `LlmRouter` 硬编码三端点（SmallModel/DefaultModel/BalancedModel），仅通过设置页换 URL |
| 改造内容 | 1. 新增 `ModelRegistry`：内置数据 + `GET /v1/models` 动态拉取<br>2. 新增 `AuthResolver`：环境变量 → 设置文件 → API Key 输入框优先级链<br>3. `list_models`/`switch_model` Tauri command 支持运行时切换 |
| 涉及文件 | `crates/llm/src/registry.rs`（新）, `crates/llm/src/auth.rs`（新）, `crates/llm/src/router.rs`, `crates/app-binary/src/commands.rs` |

### 2.8 模型参数补齐

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/common/src/config.rs:36-58`, `crates/llm/src/client.rs:77-86` |
| 问题 | 仅支持 `max_tokens` + `temperature`；缺少 `top_p`, `top_k`, `frequency_penalty`, `presence_penalty`, `stop`, `seed`, `response_format` |
| 改造内容 | `ModelEndpoint` 和 `OpenAiRequest` 补齐缺失参数 |
| 涉及文件 | `crates/common/src/config.rs`, `crates/llm/src/client.rs` |

### 2.9 流式请求超时分离

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:154-159` |
| 问题 | 全局 `reqwest::Client` timeout 覆盖流式和非流式请求；流式请求可能运行数分钟但用同一超时 |
| 改造内容 | 非流式用 endpoint timeout；流式用 `timeout_streaming_secs` 或 None（直到 SSE 结束） |
| 涉及文件 | `crates/llm/src/client.rs`, `crates/common/src/config.rs` |

### 2.10 流式请求零重试

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/router.rs:118-142` |
| 问题 | `chat_stream()` 路径完全无重试——primary 或 fallback 均不重试 |
| 改造内容 | 流式请求失败时重新发起（可能丢弃已有 chunk，重新构建 stream） |
| 涉及文件 | `crates/llm/src/router.rs` |

### 2.11 Fallback 端点无重试

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/router.rs:69-86, 95-116` |
| 问题 | `chat()` / `chat_with_tools()` 在 primary 重试 3 次后 fallback 只有 1 次机会，无重试包覆 |
| 改造内容 | Fallback 端点也接入 `with_retry` |
| 涉及文件 | `crates/llm/src/router.rs` |

### 2.12 路由器整体超时缺失

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/router.rs:69-206` |
| 问题 | Primary 4 次尝试 × 60s + Fallback 1 次 × 60s = 最长 300s 无上界 |
| 改造内容 | 新增 `max_total_duration` 参数（默认 180s），超时返回 `LlmError::Timeout` |
| 涉及文件 | `crates/llm/src/router.rs`, `crates/common/src/config.rs` |

### 2.13 主端点错误在 Fallback 失败时丢失

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/router.rs:80-84, 107-113` |
| 问题 | Primary 失败时原错误 `e` 被丢弃；若 Fallback 也失败，调用方只看到 Fallback 的错误 |
| 改造内容 | 返回复合错误或 `tried_fallback: true` 标记 |
| 涉及文件 | `crates/llm/src/router.rs`, `crates/llm/src/types.rs` |

### 2.14 Usage / Model 字段缺失

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **低** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/types.rs:34-39, 20-24` |
| 问题 | `LlmResponse` 无 `model` 字段；`Usage` 无 `model_name`/`cost`，无法区分哪个模型消费了 token |
| 改造内容 | 补充字段 |
| 涉及文件 | `crates/llm/src/types.rs`, `crates/llm/src/client.rs` |

### 2.15 Auth Header 可定制

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **低** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/llm/src/client.rs:166` |
| 问题 | `"Bearer {}"` 硬编码；`X-API-Key` 等非标准认证方式不支持 |
| 改造内容 | `ModelEndpoint` 新增 `auth_header_name` 和 `auth_header_prefix` |
| 涉及文件 | `crates/common/src/config.rs`, `crates/llm/src/client.rs` |

### 2.16 结构化解析（FinishReason 枚举化 + jsonschema 运行时校验）

| 来源 | Oh My Pi |
|------|----------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| OMP 做了什么 | 使用 `typebox` 声明工具参数 schema，`Type.Object()` 运行时校验 + TypeScript 类型推导；`ast_edit` 使用 hashline（内容哈希锚点）替代字符串匹配，避免 whitespace 冲突 |
| Haven 现状 | `tool_input: Value` (serde_json::Value) 无类型安全，LLM 输出"5"但 schema 要求 int 5 时无强制转换 |
| 改造内容 | 1. 引入 `jsonschema` crate 做运行时校验<br>2. 对多步 ReAct 的 tool_call 参数做类型强制（`String → i64`, `String → f64`）<br>3. 参考 Oh My Pi 的 hashline 思想：对文件编辑工具引入 `content_hash` 锚点验证 |
| 涉及文件 | `crates/tools/Cargo.toml`, `crates/tools/src/tool.rs`, `crates/agent/src/lib.rs` |

---

## 3. 上下文与记忆管理

### 3.1 上下文自动压缩

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| Pi 做了什么 | Token 感知截断：反向遍历消息累计 token 到 `keepRecentTokens`（默认 20k）；在轮次边界截断；调用独立 LLM 摘要；溢出时 auto-compact + 同一 prompt 重试 |
| Haven 现状 | 无压缩机制。消息累积到 50 条后被硬删除（`memory/src/repositories/messages.rs:34-40`），无摘要保存 |
| 改造内容 | 1. 新增 `ContextCompactor`：每个 step 后估算 token 数<br>2. 超过 `context_window - reserve_tokens` 时触发压缩<br>3. 对 oldest 消息调用 DefaultModel 生成摘要<br>4. `ContextLengthExceeded` 错误自动 compact + 重试 |
| 实现详情 | `crates/agent/src/compactor.rs`（`ContextCompactor` 结构体：token 估算、LLM 摘要压缩、`needs_compaction`/`compact` 方法；`ContextLengthExceeded` 时强制 compact+重试）；`maybe_compact` 在 ReAct 循环每步 LLM 调用前检查；`emit_compaction` 事件 |
| 涉及文件 | `crates/agent/src/compactor.rs`（新）, `crates/agent/src/lib.rs`, `crates/llm/src/router.rs` |

### 3.2 Session Compaction 持久化

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/memory/src/repositories/messages.rs:34-40` |
| 问题 | 滑动窗口外消息被硬删除；无 compaction 摘要保存；被删消息永久丢失 |
| 改造内容 | 1. `messages` 表新增 `is_compacted` / `compaction_id` 列<br>2. 压缩时生成 CompactionEntry（摘要 + firstKeptEntryId + tokensBefore）<br>3. 会话重载时用摘要替换原始消息范围 |
| 实现详情 | `crates/memory/src/migrations.rs`（`is_compacted`/`compaction_id` 列 + `compaction_entries` 表）；`crates/memory/src/repositories/compaction.rs`（`save_compaction`/`get_session_compactions`/`load_messages_with_compaction`）；`messages.rs`（Message 结构体+查询添加 compaction 字段） |
| 涉及文件 | `crates/memory/src/migrations.rs`, `crates/memory/src/repositories/compaction.rs`（新）, `crates/memory/src/repositories/messages.rs` |

### 3.3 ReAct Step 历史表 Schema 修正

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/memory/src/migrations.rs:28-42`, `crates/agent/src/lib.rs:370-372` |
| 问题 | Thought 被当作 `tool_name = "thought"` 的伪工具步骤存储；`TaskStep` struct 无 `thought` 字段 |
| 改造内容 | `task_steps` 表拆分为 `thought`, `action_tool`, `action_input`, `observation` 列 |
| 实现详情 | `migrations.rs`（四列+数据回填）；`task_steps.rs`（`TaskStep` 结构体拆分, `create_thought_step`/`create_action_step`/`complete_action_step` 方法, 向后兼容旧数据, 完整测试）；`agent/src/lib.rs`（改用 `create_thought_step`）；`task/src/lib.rs`（改用 `create_action_step`） |
| 涉及文件 | `crates/memory/src/migrations.rs`, `crates/memory/src/repositories/task_steps.rs`, `crates/agent/src/lib.rs`, `crates/task/src/lib.rs` |

### 3.4 偏好推断增强

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/memory/src/repositories/preferences.rs:80-93` |
| 问题 | 仅推断最常用工具（简单频率）；缺失：常用文件路径、常用应用、任务类型模式、响应语言偏好 |
| 改造内容 | 每日任务时间分布分析、基于 `facts` 三元组的自动知识提取、常用路径/命令频率统计 |
| 实现详情 | 1. `infer_preferences_from_messages` 规则引擎支持语言/工作目录/编辑器/详略度/工具偏好推断（`preferences.rs:175-303`）<br>2. `record_tool_usage` 追踪工具调用频次及参数模式统计（`preferences.rs:141-169`）<br>3. `infer_facts_from_messages` 规则引擎提取用户事实三元组（`facts.rs:192-313`）<br>4. `get_preference_summary` 结构化输出注入 system prompt（`agent/src/lib.rs:306-316`）<br>5. 完整单元测试覆盖所有推断路径（`preferences.rs:356-615`, `facts.rs`）<br>6. `save_inferred_preferences` 尊重用户手动设置优先级（`preferences.rs:308-318`） |
| 涉及文件 | `crates/memory/src/repositories/preferences.rs`, `crates/memory/src/repositories/facts.rs`, `crates/agent/src/lib.rs:211-218, 290-316, 1215-1238` |

### 3.5 Session Tree 结构

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| Pi 做了什么 | JSONL append-only，每条 entry 有 `id + parentId`；支持分支、回退、compaction 节点；`buildSessionContext()` leaf→root 遍历 |
| Haven 现状 | SQLite 单表线性会话，无分支/回退 |
| 改造内容 | 1. `sessions` 表加 `parent_id` 列<br>2. `build_session_context()` leaf→root 遍历 + compaction 解析<br>3. UI `/tree` 命令分支跳转 |
| 实现详情 | `migrations.rs`（`parent_id` 列）；`sessions.rs`（`create_session(parent_id)`/`branch_session`/`build_session_context` leaf→root 遍历, 测试） |
| 涉及文件 | `crates/memory/src/migrations.rs`, `crates/memory/src/repositories/sessions.rs` |

### 3.6 Hindsight：Agent 自维护记忆（retain/recall/reflect）

| 来源 | Oh My Pi |
|------|----------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| OMP 做了什么 | Agent 运行时通过 `retain` 写入事实，`recall` 搜索记忆库，`reflect` 跨记忆合成回答；会话间自动压缩成 mental model，新会话首 turn 自动加载；project-scoped |
| Haven 现状 | `crates/memory/src/repositories/preferences.rs` 仅做简单频率统计，无 Agent 驱动的记忆写入/检索 |
| 改造内容 | 1. 新增 `HindsightStore`：`{ key, content, embedding?, tags[], session_id, created_at }`<br>2. 内置 `retain` 工具：Agent 自主写入事实<br>3. 内置 `recall` 工具：关键词/语义搜索记忆<br>4. 新会话启动时自动加载当前 project 的活跃记忆摘要<br>5. 会话结束时自动压缩关键信息到记忆库 |
| 实现详情 | `crates/memory/src/hindsight.rs`（`retain_hindsight`/`recall_hindsight`/`recall_by_key`/`forget_hindsight`/`hindsight_summary`, 完整测试）；`migrations.rs`（`hindsight_store` 表+索引） |
| 涉及文件 | `crates/memory/src/hindsight.rs`（新）, `crates/memory/src/migrations.rs` |

### 3.7 流规则（Time-Traveling Stream Rules）

| 来源 | Oh My Pi |
|------|----------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| OMP 做了什么 | 规则通过正则匹配模型输出流；匹配时中止当前 token 流，注入规则作为 system reminder，从断点重试补偿；注入 survive compaction；规则只在实际越界时触发，不增加每 turn 的 context tax |
| Haven 现状 | 无流级规则注入；所有规则硬编码在 system prompt 中（每 turn 浪费 token） |
| 改造内容 | 1. 新增 `StreamRule` 结构：`{ pattern: Regex, inject: String, mode: abort | warn }`<br>2. 在 `aggregate_stream_cancellable` 中每收到 chunk 做 pattern 匹配<br>3. 匹配 abort 模式时：中断当前流 → 注入规则 → 重试 same prompt<br>4. 匹配 warn 模式时：仅 emit 事件到 UI |
| 实现详情 | `crates/llm/src/stream_rules.rs`（`StreamRule` 结构体, `check_stream_rules` 引擎, abort/warn 模式, 完整测试）；`router.rs`（流规则字段+`set_stream_rules`/`check_stream_output` 方法） |
| 涉及文件 | `crates/llm/src/stream_rules.rs`（新）, `crates/llm/src/router.rs` |

---

### 4.1 Per-Tool 超时

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/tools/src/tool.rs:37`, `crates/tools/src/builtin/mod.rs:42-45, 124-128, 186-190` |
| 问题 | `Tool::execute` trait 无 timeout 参数；三个内置工具完全忽略 `_cancel: CancellationToken`；文件读取/进程列表可无限阻塞 |
| 改造内容 | 1. `Tool` trait 新增 `execute_with_timeout()`<br>2. 每个工具在 `50ms + tool_config.timeout` 内完成<br>3. 内置工具实现 CancellationToken 检查 |
| 涉及文件 | `crates/tools/src/tool.rs`, `crates/tools/src/builtin/mod.rs`, `crates/common/src/config.rs` |

### 4.2 Tool 结果 Size Limit

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/tools/src/tool.rs:15-20`, `crates/tools/src/builtin/mod.rs:51, 82-86, 132-135` |
| 问题 | `ToolResult.output: Value` 无大小上限；文件读取全量加载到内存；目录列表无边界 |
| 改造内容 | 1. `ToolResult` 新增 `truncated: bool`<br>2. 读取/列表操作加 `max_output_chars`（默认 100k）<br>3. 超出部分截断 + 尾部标注 `[truncated ... N chars omitted]` |
| 涉及文件 | `crates/tools/src/tool.rs`, `crates/tools/src/builtin/mod.rs`, `crates/common/src/config.rs` |

### 4.3 Tool 结果上下文截断

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/agent/src/lib.rs:708-716` |
| 问题 | 完整 `step_result` 无条件追加到 `messages`；无截断、无摘要、无最大字符限制 |
| 改造内容 | 每个 tool result 推入 messages 前截断到 `max_observation_chars`（默认 8000）并追加 `[... truncated]` |
| 涉及文件 | `crates/agent/src/lib.rs:708-716` |

### 4.4 Tool Call 参数验证

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/agent/src/lib.rs:662-665`, `crates/task/src/lib.rs:595-599` |
| 问题 | LLM 输出的 `tool_input` 直接传入工具执行，无 schema 校验、无必需参数检查、无类型强制转换、无路径穿越安全检查 |
| 改造内容 | 1. 执行前校验 `tool_input` 符合 `input_schema`（用 `jsonschema` crate）<br>2. 类型强制（LLM 输出 `"5"` 但 schema 要求 int 5）<br>3. 路径参数过滤 `../` 和绝对路径 |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/tools/src/tool.rs`（新增 `validate_input()` 方法） |

### 4.5 MCP Tool 速率限制

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/tools/src/mcp/mod.rs:321-373, 723-735` |
| 问题 | `McpClient::call_tool` 无速率限制；紧密 ReAct 循环可短时间内发起几十个 MCP 调用 |
| 改造内容 | Per-server token bucket（`calls_per_second` 可配置） |
| 涉及文件 | `crates/tools/src/mcp/mod.rs`, `crates/common/src/config.rs` |

### 4.6 MCP Server-Pushed Tool Changes

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/tools/src/mcp/mod.rs:108-126` |
| 问题 | `notifications/tools/list_changed` 未被处理；server-pushed 工具变更被静默丢弃 |
| 改造内容 | 分离通知通道：非匹配 request ID 的消息路由到 `notification_handler` |
| 涉及文件 | `crates/tools/src/mcp/mod.rs` |

### 4.7 Progressive Skill Loading

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **中** |
| 状态 | ✅ 已完成 |
| Pi 做了什么 | 启动时仅提取 name + description 注入索引；需要时 LLM 通过 read 工具自行加载完整 SKILL.md；`disable_model_invocation: true` 隐藏 |
| Haven 现状 | `build_tool_definitions()` 将所有工具完整 schema 注入 system prompt |
| 改造内容 | 1. `build_skill_index()` — 提取 name + description<br>2. 新增 `load_skill` 内置工具<br>3. System prompt 仅注入 skill 索引列表 |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/tools/src/skills/` |

### 4.8 Per-Tool 设置

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 状态 | ✅ 已完成 |
| 问题位置 | `crates/tools/src/tool.rs:33-39`, `crates/common/src/config.rs:282-297` |
| 问题 | `Tool` trait 无 config/settings 方法；无法配置允许路径、最大文件大小、禁用操作 |
| 改造内容 | 1. `AppConfig` 新增 `tool_settings: HashMap<String, ToolConfig>`<br>2. `ToolConfig { timeout_secs, max_output_chars, allowed_paths, disabled_operations, risk_override }` |
| 涉及文件 | `crates/common/src/config.rs`, `crates/tools/src/tool.rs`, `crates/tools/src/builtin/mod.rs` |

---

### 5.1 指数退避 + Jitter（Agent 级重试）

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **中** |
| Pi 做了什么 | `baseDelayMs=2000, factor=2, maxRetries=3, jitter=0.2`；`auto_retry_start`/`auto_retry_end` 事件 |
| Haven 现状 | `with_retry` 硬编码 `2^attempt` 秒，无 jitter |
| 改造内容 | 1. 退避参数从 `ModelEndpoint` 读取<br>2. `backoff = min(base * factor^attempt + jitter*base, max_delay)`<br>3. 发 `auto_retry_start`/`auto_retry_end` 事件到 UI |
| 涉及文件 | `crates/llm/src/client.rs:438-463`, `crates/llm/src/router.rs`, `crates/common/src/config.rs` |

### 5.2 Compaction-Retry

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **高** |
| Pi 做了什么 | LLM 返回 context overflow → 自动 compact → 同一 prompt 重试；`compaction_end { willRetry: true }` 事件 |
| Haven 现状 | `ContextLengthExceeded` 错误变体不存在，无法触发此流程 |
| 改造内容 | 1. 识别 `ContextLengthExceeded` → 触发 §3.1 压缩 → 重试<br>2. 重试次数上限 2 次 |
| 涉及文件 | `crates/agent/src/compactor.rs`, `crates/llm/src/router.rs`, `crates/llm/src/types.rs` |

### 5.3 Fallback 活跃状态竞争条件

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/llm/src/router.rs:76-84` |
| 问题 | 单个 `AtomicBool` 表示 3 个端点健康状态；并发调用可能覆盖彼此的状态 |
| 改造内容 | Per-endpoint 状态：`EndpointHealth { consecutive_failures, last_failure_time, is_healthy }` |
| 涉及文件 | `crates/llm/src/router.rs` |

### 5.4 后台健康检查

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **低** |
| 问题位置 | `crates/llm/src/router.rs:258-261` |
| 问题 | `health_check` 可手动调用但无后台 polling；无法主动探测端点恢复 |
| 改造内容 | 每 60s 对 fallback 端点做 health check；主端点恢复后自动切回 |
| 涉及文件 | `crates/llm/src/router.rs` |

### 5.5 连接池调优

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **低** |
| 问题位置 | `crates/llm/src/client.rs:154-159` |
| 问题 | `reqwest::Client` 使用默认连接池参数；对 LLM API 模式未优化 |
| 改造内容 | 设 `pool_max_idle_per_host(5)`, `pool_idle_timeout(90s)` |
| 涉及文件 | `crates/llm/src/client.rs` |

### 5.6 协作会话（Collab Session）

| 来源 | Oh My Pi |
|------|----------|
| 严重度 | **低** |
| OMP 做了什么 | `/collab` 将活跃会话发布到 relay，返回链接 + QR；`omp join` 从另一终端加入；`/collab view` 只读模式；帧在客户端加密，relay 不暴露 API keys |
| Haven 现状 | 无远程协作能力 |
| 改造内容 | 1. 新增 `crates/collab/` crate：WebSocket relay 客户端<br>2. `BridgeEvent` 新增 `CollabJoin` / `CollabLeave` / `CollabFrame` 变体<br>3. 可配置 relay URL（默认自建或公共 relay）<br>4. 会话帧在发送前使用 session key 加密 |
| 涉及文件 | `crates/collab/`（新 crate）, `crates/bridge/src/lib.rs`, `crates/app-binary/src/lib.rs` |

---

## 6. 配置与可扩展性

### 6.1 AGENTS.md 发现与注入

| 来源 | Pi 框架 |
|------|---------|
| 严重度 | **低** |
| Pi 做了什么 | 从 cwd 向上递归加载 `AGENTS.md` / `CLAUDE.md`，注入 system prompt |
| Haven 现状 | Kilo 已加载 AGENTS.md，Haven Agent 自身不感知 |
| 改造内容 | `AgentLayer` 启动时从工作目录向上扫描 `.haven/AGENTS.md` 并注入 system prompt |
| 涉及文件 | `crates/agent/src/lib.rs` |

### 6.2 Proxy 设置 UI

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/common/src/config.rs:36-58, 282-297` |
| 问题 | `ModelEndpoint` 和 `AppConfig` 无 proxy 字段 |
| 改造内容 | `ModelEndpoint` 加 `proxy_url`, `no_proxy`；设置页面新增 Proxy 配置 |
| 涉及文件 | `crates/common/src/config.rs`, 设置页面 Svelte 组件 |

### 6.3 Per-Tool 设置 UI

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题 | 同 §4.8，UI 层面无工具配置入口 |
| 改造内容 | 设置页面新增 "Tools" 标签，列出所有工具及其可配置参数 |
| 涉及文件 | `ui/src/routes/settings/` |

---

## 7. UI 事件桥接

### 7.1 Compaction 事件

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题位置 | `crates/agent/src/lib.rs:43-63` |
| 问题 | `AgentEventEmitter` trait 无 `on_compaction` 方法；上下文被压缩时 UI 无感知 |
| 改造内容 | 新增 `on_compaction(task_id, old_len, new_len, summary)` 事件 |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/app-binary/src/lib.rs` |

### 7.2 Cost / Usage 事件

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/agent/src/lib.rs:568-601`, `crates/llm/src/types.rs:37-39` |
| 问题 | `LlmResponse.usage` 已获取但从不发送到 UI；无 token 消耗追踪 |
| 改造内容 | 1. `AgentEventEmitter` 新增 `on_usage(task_id, prompt_tokens, completion_tokens, model, cost)` <br>2. 每个 Reasoner 响应后 emit usage |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/app-binary/src/lib.rs` |

### 7.3 Model Recovery 事件

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/app-binary/src/lib.rs:145-153` |
| 问题 | 有 `on_fallback_activated` 但无 `on_fallback_deactivated` |
| 改造内容 | 新增 `on_fallback_deactivated(task_id, model)` 事件 |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/app-binary/src/lib.rs` |

### 7.4 Tool Catalog Change 事件

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/tools/src/lib.rs:76-107` |
| 问题 | `rebuild_catalog()` 无事件发送；前端只能通过 polling 感知工具变化 |
| 改造内容 | 新增 `on_tool_catalog_changed(added, removed)` 事件 |
| 涉及文件 | `crates/tools/src/lib.rs`, `crates/app-binary/src/lib.rs` |

### 7.5 Session Lifecycle 事件

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **低** |
| 问题位置 | `crates/memory/src/repositories/sessions.rs:14-28` |
| 问题 | `create_session` 无事件发送；无 `session:created` / `session:closed` |
| 改造内容 | 新增 session 生命周期事件 |
| 涉及文件 | `crates/memory/src/repositories/sessions.rs`, `crates/app-binary/src/lib.rs` |

---

## 8. 测试覆盖

### 8.1 Memory Crate 零测试

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题位置 | `crates/memory/src/` (全部文件) |
| 问题 | 整个 `haven-memory` crate 无任何 `#[cfg(test)]` 块；所有 repository、migration、db 逻辑未测试 |
| 改造内容 | 每 repository 新增 unit test：CRUD 操作、边界条件、错误路径、schema 迁移 |
| 涉及文件 | `crates/memory/src/repositories/*.rs`, `crates/memory/src/migrations.rs`, `crates/memory/src/db.rs` |

### 8.2 内置工具零测试

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题位置 | `crates/tools/src/builtin/mod.rs` |
| 问题 | FileOpTool, ProcessTool, ClipboardTool 均无 unit test |
| 改造内容 | 每工具新增 test：正常执行、错误输入、取消、超时、权限拒绝 |
| 涉及文件 | `crates/tools/src/builtin/mod.rs` |

### 8.3 Agent 集成测试不足

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题位置 | `crates/agent/src/lib.rs:884-928` |
| 问题 | 仅一个测试（`run_task_emits_supplement`），使用 `FinalAnswerMock`（单步终止）；无多步、fallback、取消、暂停恢复 |
| 改造内容 | 新增测试：multi-step ReAct, fallback 切换, 取消中循环, 暂停恢复, classify→dispatch→execute 全流程 |
| 涉及文件 | `crates/agent/src/lib.rs` |

### 8.4 Mock Provider 不足

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/agent/src/lib.rs:808-858`, `crates/llm/src/router.rs:278-317` |
| 问题 | 仅 FinalAnswerMock 和 MockStreamClient；缺失：429 模拟、401 模拟、部分 tool_call、空响应、畸形 JSON |
| 改造内容 | 新增 ErrorMock（返回指定 error）、MultiToolMock（返回多个 tool_call）、PartialStreamMock（中途断开） |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/llm/src/router.rs` |

### 8.5 MCP 集成测试缺失

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题位置 | `crates/tools/src/mcp/mod.rs:748-816` |
| 问题 | 仅序列化/往返测试；无启动真实 MCP server 的连接测试 |
| 改造内容 | 集成测试：启动 MCP server → 连接 → 列出工具 → 调用 → 处理错误 |
| 涉及文件 | `crates/tools/src/mcp/mod.rs` |

### 8.6 App-Binary 零测试

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题位置 | `crates/app-binary/src/` (全部文件) |
| 问题 | Tauri shell 代码零测试覆盖 |
| 改造内容 | EventEmitter 单元测试、Commands 参数验证测试 |
| 涉及文件 | `crates/app-binary/src/lib.rs`, `crates/app-binary/src/commands.rs` |

---

## 9. 安全加固

### 9.1 安全网关集成到 ReAct 循环

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题位置 | `crates/agent/src/lib.rs:662-665`, `crates/task/src/lib.rs:533-593` |
| 问题 | 安全确认逻辑仅在 `execute_task()`（旧路径）完整实现；ReAct 循环的 `execute_step()` 绕过 `SafetyGateway::check()` |
| 改造内容 | 1. `execute_step()` 中插入 SafetyGateway check<br>2. 高风险操作通过 `on_confirm_request` 事件发到 UI<br>3. 30s 超时 + 拒绝/超时则 skip 该 step |
| 涉及文件 | `crates/agent/src/lib.rs`, `crates/task/src/lib.rs:394-447` |

### 9.2 Path Traversal 防护

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **高** |
| 问题 | §4.4 / §9.1 同一问题链 — 无路径验证 |
| 改造内容 | 1. 所有文件操作参数 normalize 后检查 `starts_with(allowed_root)`<br>2. 拒绝 `..` 遍历和绝对路径（不存在于白名单中的） |
| 涉及文件 | `crates/tools/src/builtin/mod.rs` |

### 9.3 Tool 执行沙盒化

| 来源 | 代码审计 |
|------|---------|
| 严重度 | **中** |
| 问题 | ProcessTool 无命令白名单；任意 `tasklist`, `taskkill` 等可造成破坏 |
| 改造内容 | `ProcessTool` 新增 `allowed_commands: HashSet<String>`；非白名单命令拒绝执行 |
| 涉及文件 | `crates/tools/src/builtin/mod.rs`, `crates/common/src/config.rs` |

---

## 10. 实施路线图

### 优先级矩阵

| 优先级 | 项目数 | 特征 |
|--------|--------|------|
| P0 (立即) | 8 | 安全漏洞 + 数据丢失风险 |
| P1 (M6-01) | 12 | 基础架构 + 安全 |
| P2 (M6-02) | 11 | 稳定性闭环 |
| P3 (M6-03) | 10 | 可靠性 + 可扩展性 |
| P4 (M6-04) | 13 | 体验优化 + 测试补齐 |

### P0：立即修复（安全 + 数据安全）

| # | 项目 | § |
|---|------|---|
| 1 | 安全网关集成到 ReAct 循环 | 9.1 |
| 2 | Path Traversal 防护 | 9.2 |
| 3 | Tool Call 参数验证 | 4.4 |
| 4 | Tool 执行沙盒化（ProcessTool 命令白名单） | 9.3 |
| 5 | Per-Tool 超时 | 4.1 |
| 6 | Tool 结果 Size Limit | 4.2 |
| 7 | 流截断检测 | 2.1 |
| 8 | 结构化错误类型 | 2.2 |

### P1：M6-01 基础架构

| # | 项目 | § |
|---|------|---|
| 1 | Provider-Neutral 消息格式 | 1.1 |
| 2 | LLM 输出结构化解析 | 1.5 |
| 3 | Rate Limit 处理（Retry-After） | 2.3 |
| 4 | 流式 Tool Call Delta 修复 | 2.4 |
| 5 | Fallback 端点重试 | 2.11 |
| 6 | Circuit Breaker | 2.6 |
| 7 | 路由器整体超时 | 2.12 |
| 8 | 主端点错误保留 | 2.13 |
| 9 | Tool Result 上下文截断 | 4.3 |
| 10 | ReAct Step 历史 Schema 修正 ✅ | 3.3 |
| 11 | AGENTS.md 注入 | 6.1 |
| 12 | Agent 状态持久化 | 1.3 |

### P2：M6-02 稳定性闭环

| # | 项目 | § |
|---|------|---|
| 1 | 上下文自动压缩 ✅ | 3.1 |
| 2 | Session Compaction 持久化 ✅ | 3.2 |
| 3 | Compaction-Retry | 5.2 |
| 4 | 双层 Agent 循环 | 1.2 |
| 5 | 多工具并行执行 | 1.4 |
| 6 | 指数退避 + Jitter | 5.1 |
| 7 | Fallback 状态竞争条件修复 | 5.3 |
| 8 | Compaction 事件 | 7.1 |
| 9 | Cost/Usage 事件 | 7.2 |
| 10 | Model Recovery 事件 | 7.3 |
| 11 | Session Lifecycle 事件 | 7.5 |

### P3：M6-03 可靠性 + 可扩展性

| # | 项目 | § |
|---|------|---|
| 1 | Progressive Skill Loading | 4.7 |
| 2 | Proxy 支持 | 2.5 |
| 3 | Proxy 设置 UI | 6.2 |
| 4 | 多模态内容支持 | 1.6 |
| 5 | 流式请求超时分离 | 2.9 |
| 6 | 流式请求重试 | 2.10 |
| 7 | MCP Tool 速率限制 | 4.5 |
| 8 | MCP Server-Pushed Tool Changes | 4.6 |
| 9 | Tool Catalog Change 事件 | 7.4 |
| 10 | 请求去重 | 2.16 |

### P4：M6-04 体验优化 + 测试

| # | 项目 | § |
|---|------|---|
| 1 | Multi-Provider 模型发现 | 2.7 |
| 2 | 模型参数补齐 | 2.8 |
| 3 | Per-Tool 设置 | 4.8 |
| 4 | Per-Tool 设置 UI | 6.3 |
| 5 | 偏好推断增强 ✅ | 3.4 |
| 6 | Session Tree 结构 ✅ | 3.5 |
| 7 | Memory Crate 测试 | 8.1 |
| 8 | 内置工具测试 | 8.2 |
| 9 | Agent 集成测试 | 8.3 |
| 10 | Mock Provider 补齐 | 8.4 |
| 11 | MCP 集成测试 | 8.5 |
| 12 | App-Binary 测试 | 8.6 |
| 13 | Auth Header 可定制 + 其他低优项 | 2.15, 2.14, 2.17, 5.4, 5.5 |

---

## 附录：审计统计

| 类别 | 总计 | ✅ 已完成 | ❌ 未开始 | P0 | P1 | P2 | P3 | P4 |
|------|------|----------|----------|----|----|----|----|----|
| 架构与核心循环 | 6 | 6 | 0 | 0 | 3 | 3 | 0 | 0 |
| LLM 与 Provider | 16 | 16 | 0 | 2 | 8 | 2 | 2 | 4 |
| 上下文与记忆 | 7 | 7 | 0 | 0 | 2 | 2 | 0 | 3 |
| 工具系统与 MCP | 10 | 8 | 2 | 4 | 1 | 0 | 3 | 2 |
| 错误恢复 | 6 | 0 | 6 | 0 | 0 | 4 | 0 | 2 |
| 配置与扩展 | 3 | 0 | 3 | 0 | 1 | 0 | 1 | 1 |
| UI 事件 | 5 | 0 | 5 | 0 | 0 | 4 | 1 | 0 |
| 测试 | 6 | 0 | 6 | 0 | 0 | 0 | 0 | 6 |
| 安全 | 3 | 0 | 3 | 3 | 0 | 0 | 0 | 0 |
| **合计** | **62** | **37** | **25** | **9** | **15** | **15** | **8** | **15** |

来源：Pi Coding Agent 框架对照 + Oh My Pi (can1357/oh-my-pi) Rust 架构对照 + `crates/` 全量代码审计（`llm`, `agent`, `task`, `tools`, `memory`, `common`, `app-binary`, `input`, `desktop`, `bridge`）

最后更新：2026-07-23（§2 LLM 与 Provider 层已补全实施状态）

---

> **§3 实现状态**：7/7 项改进全部完成 (2026-07-23)
>
> | 项目 | 状态 | 变更文件 |
> |------|------|----------|
> | 3.1 上下文自动压缩 | ✅ 已完成 | `crates/agent/src/compactor.rs`（新建：`ContextCompactor` 结构体，token 估算，LLM 摘要压缩，`ContextLengthExceeded` 自动 compact+重试，`maybe_compact` 集成到 ReAct 循环） |
> | 3.2 Session Compaction 持久化 | ✅ 已完成 | `crates/memory/src/migrations.rs`（`is_compacted`/`compaction_id` 列 + `compaction_entries` 表）, `crates/memory/src/repositories/compaction.rs`（新建：`save_compaction`/`get_session_compactions`）, `crates/memory/src/repositories/messages.rs`（Message 结构体+查询添加 compaction 字段） |
> | 3.3 ReAct Step 历史表 Schema 修正 | ✅ 已完成 | `crates/memory/src/migrations.rs`（`thought`/`action_tool`/`action_input`/`observation` 列 + 数据回填）, `crates/memory/src/repositories/task_steps.rs`（`TaskStep` 结构体拆分, `create_thought_step`/`create_action_step`/`complete_action_step` 方法, 向后兼容, 完整测试）, `crates/agent/src/lib.rs`（改用 `create_thought_step`）, `crates/task/src/lib.rs`（改用 `create_action_step`） |
> | 3.4 偏好推断增强 | ✅ 已完成 | `crates/memory/src/repositories/preferences.rs`（语言/目录/编辑器/详略度推断、工具使用频次统计、偏好摘要与系统提示注入、完整测试覆盖）, `crates/memory/src/repositories/facts.rs`（三元组规则推断）, `crates/agent/src/lib.rs:290-316, 1215-1238`（系统提示注入 + 推理触发） |
> | 3.5 Session Tree 结构 | ✅ 已完成 | `crates/memory/src/migrations.rs`（`parent_id` 列）, `crates/memory/src/repositories/sessions.rs`（`create_session(parent_id)`/`branch_session`/`build_session_context` leaf→root 遍历, 测试） |
> | 3.6 Hindsight | ✅ 已完成 | `crates/memory/src/hindsight.rs`（新建：`retain_hindsight`/`recall_hindsight`/`recall_by_key`/`forget_hindsight`/`hindsight_summary` 方法, 完整测试）, `crates/memory/src/migrations.rs`（`hindsight_store` 表 + 索引） |
> | 3.7 流规则 | ✅ 已完成 | `crates/llm/src/stream_rules.rs`（新建：`StreamRule` 结构体, `check_stream_rules` 引擎, abort/warn 模式, 完整测试）, `crates/llm/src/router.rs`（流规则字段+`set_stream_rules`/`check_stream_output` 方法） |

---

> **§4 实现状态**：8/9 项改进完成，4.9 按设计约束不实施 (2026-07-22)
>
> | 项目 | 状态 | 变更文件 |
> |------|------|----------|
> | 4.1 Per-Tool 超时 | ✅ | `crates/tools/src/tool.rs` (execute_with_timeout), `crates/tools/src/builtin/mod.rs` (CancellationToken 检查), `crates/tools/src/lib.rs` (execute_tool 集成 timeout) |
> | 4.2 Tool 结果 Size Limit | ✅ | `crates/tools/src/tool.rs` (ToolResult.truncated), `crates/tools/src/builtin/mod.rs` (max_output_chars 截断) |
> | 4.3 Tool 结果上下文截断 | ✅ | `crates/agent/src/lib.rs` (MAX_OBSERVATION_CHARS=8000) |
> | 4.4 Tool Call 参数验证 | ✅ | `crates/tools/src/tool.rs` (validate_input: jsonschema), `crates/tools/src/builtin/mod.rs` (sanitize_path 路径遍历防护) |
> | 4.5 MCP Tool 速率限制 | ✅ | `crates/tools/src/mcp/mod.rs` (RateLimiter token bucket, set_rate_limit) |
> | 4.6 MCP Server-Pushed Tool Changes | ✅ | `crates/tools/src/mcp/mod.rs` (notification_rx 通道, start_notification_listener, 处理 `notifications/tools/list_changed`) |
> | 4.7 Progressive Skill Loading | ✅ | `crates/tools/src/builtin/mod.rs` (LoadSkillTool), `crates/tools/src/lib.rs` (build_skill_index, rebuild_catalog 仅注入索引), `crates/agent/src/lib.rs` (system prompt 展示 skill 索引) |
> | 4.8 Per-Tool 设置 | ✅ | `crates/common/src/config.rs` (ToolConfig, tool_settings in AppConfig/Settings/apply_settings) |
> | 4.9 结构化工具结果 | ❌ | 按设计约束不实施（过度设计，参见 `no_overdesigned_features`） |
