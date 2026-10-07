# ADR 0737：推理强度 setter 类型化并保留开放 provider 配置

## 状态

已采纳并实施。

## 背景

`set_reasoning_effort` 的唯一 production caller 是聊天页 `ModelToolbar`，其固定选项为默认、`low`、`medium`、`high`、`off`。`chatModelOperations.selectEffort` 将默认项映射为 `null`，命令成功后才更新当前 UI 状态；失败时保留旧状态并报告错误。

App handler 原先接受 `Option<String>`，把空白归一为 `None`，其它值 trim 后写进当前 `request_kind` 对应的 ModelConfig；runtime config coordinator 串行持久化并 apply LlmRouter。成功后发送 `llm:config_changed`。ModelConfig 与 ModelEndpoint 都保留 `Option<String>`，provider adapters 对值做不同处理：DeepSeek/Kimi 会映射 `xhigh` / `max` / disable tokens，Anthropic 使用其 thinking/effort 规则，OpenAI-compatible 与 Responses 路径会按 provider policy 传递相应值。配置中的 provider-specific values 因此是开放边界。

## 决定

- 新增 App command input enum `ReasoningEffortSelection::{Low, Medium, High, Off}`，`set_reasoning_effort` 接受 `Option<ReasoningEffortSelection>` 并生成 `ReasoningEffortSelectionInput`。
- App 将 enum 映射到既有配置文本 `low` / `medium` / `high` / `off`；`None` 继续清除当前模型覆盖。工具栏选项与 callback/controller 引用生成类型，避免 UI 将其它 provider 配置 token 发入 setter。
- 不收窄 `ModelConfig.reasoning_effort`、`ModelEndpoint.reasoning_effort` 或 LLM adapter 输入。配置文件中的 `max`、`xhigh`、`none`、`disabled` 等值继续沿现有 provider adapter policy 处理；不改变现有配置、model route 或 hot-swap 逻辑。

## 替代方案

- 保留 setter 的开放字符串：拒绝。唯一 UI caller 的选择集固定，开放命令类型不代表可用 provider 设置能力，因为 `ModelToolbar` 没有这些自定义项。
- 将 ModelConfig 与 LLM adapter 一并改为 enum：拒绝。provider vocabulary 与 adapter 行为不同，配置边界必须保留 raw provider-specific value。
- 用 `off` 替代 `null` 清除：拒绝。`off` 是明确配置值，会覆盖 provider/environment defaults；`null` 删除模型 override。

## 影响与验证

Generated command request 从 `effort?: string | null` 收窄为 `effort?: ReasoningEffortSelectionInput | null`。UI 原有选择值不变，`null` 仍清除；空字符串和 `max` / `xhigh` / `none` 等值只在 command setter 被拒绝，不影响 ModelConfig 持久值和 provider adapter。无数据库 schema、配置文件字段、迁移或数据重置变化。配置 apply 失败仍由命令返回错误，前端不先行更新当前状态。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 992 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（720 records）及 `git diff --check` 通过。

## 回滚

如回滚，需恢复 `set_reasoning_effort` 的 `Option<String>`、空白归一逻辑、generated request、工具栏/controller 类型、相关文档和本 ADR 索引；配置 schema 与 provider adapters 无需修改。
