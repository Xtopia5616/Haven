# ADR 0437：聊天页模型选择器使用已配置路由模型

- 状态：已采纳（2026-10-03）
- 范围：聊天页模型选择、`switch_model` 命令语义与 Chat 路由同步

## 背景

设置页用 `RequestPolicy.primary` 选择具名 `ModelConfig`，但聊天页模型菜单却从当前 Provider 动态发现
服务模型，并通过 `switch_model(role="chat", modelId=...)` 改写当前 `ModelConfig.model`。两处 UI 操作的
对象不同：设置页选择路由目标，聊天页编辑目标配置内部的 Provider 模型；聊天页不能在已配置模型之间切换，
还可能使配置页的模型标识与聊天页所表达的“模型切换”语义混淆。

## 决定

- 聊天页模型菜单从 `get_settings` 中读取已配置模型，列出声明 `chat` 能力且已绑定 Provider 和服务模型 ID 的
  `ModelConfig`。菜单项以配置 ID 为稳定标识，并展示 Provider 与服务模型 ID。
- 选择菜单项时，`switch_model` 将 `role` 解析为 `RequestKind`，将 `modelId` 解析为具备所需能力且配置完整的
  `ModelConfig.id`，然后通过配置 apply coordinator 更新对应的 `RequestPolicy.primary`。
- 模型配置本身仍由设置页编辑；聊天页选择只切换 Chat route primary，不修改 `ModelConfig.model`、Provider 或
  模型级参数。思考强度和联网搜索状态随选中的模型配置更新。
- 这是全局默认路由选择，不是当前 session 的模型覆盖。每个 `RequestKind` 仍只有一个 primary，不恢复 provider/model
  failover，也不改变 Router 的重试和熔断策略。
- 切换会影响所有会话后续发出的对话请求；已开始的模型调用继续按发起时的路由完成。模型列表作为主操作展示，思考强度和
  联网搜索折叠为高级选项；模型配置较多时可搜索配置名、Provider 和服务模型 ID。
- `switch_model` 的 Tauri 字段名与 wire shape 不变；`modelId` 的语义从 Provider 服务模型 ID 改为具名模型配置 ID。

## 替代方案

- 继续在聊天页改写当前模型配置：会继续保留两套模型选择语义，拒绝。
- 在聊天页加入当前 session 的模型覆盖：这需要 session 级状态、恢复语义和调用请求 DTO 变化，超出本次全局默认路由
  统一范围。
- 继续即时发现 Provider 服务模型供聊天菜单选择：模型发现属于 Provider/模型配置编辑流程；聊天菜单只选择已配置的
  路由目标。

## 影响与验证

- 请求配置结构和持久化 shape 不变，不需要重置或迁移 `config.toml`。
- 后端验证 request kind、模型配置存在、Provider/model 绑定完整以及 capability 匹配；前端只显示可配置为 Chat
  route 的模型配置。
- 回归覆盖 route primary 更新、不改 Provider 服务模型 ID、拒绝缺失或 capability 不匹配的模型配置，以及聊天页选项和
  选中状态均以 `ModelConfig.id` 表达；普通模型选择与低频参数调整分层展示。
- 验证命令：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、
  `cargo test --workspace --locked`、`corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run`、
  `corepack pnpm --dir ui run build`、`pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`。

## 回滚

回滚本提交即可恢复聊天页在当前 Provider 服务模型之间切换的旧语义；配置 schema 与已保存数据无需迁移。
