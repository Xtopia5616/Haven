# ADR 0323：模型配置完整应用归 RuntimeConfigCoordinator

- 状态：已采纳（2026-09-25）
- 范围：`haven-app-binary` 的 model 命令配置提交与 Router runtime 应用
- 关联：[ADR 0235](0235-versioned-runtime-config-apply-boundary.md)、[ADR 0253](0253-runtime-config-coordinator.md)

## 背景

ADR 0253 已把 Router runtime 的 prepare/publish 与 settings/model 共用 gate 移入
`RuntimeConfigCoordinator`，但 `commands/model.rs` 仍自行取得 gate、调用 `ConfigService::edit`、
根据 `RuntimeConfigApplyPlan` 判断 Router target，再调用 coordinator 的 Router apply。模型命令因此仍
重复编排配置提交和 live apply 的边界。

## 决定

- `RuntimeConfigCoordinator::edit_model_and_apply` 成为模型配置完整应用的唯一协调入口。它持有共享 gate，
  调用 `ConfigService::edit` 获得持久化后的 snapshot/change，根据 change 的
  `RuntimeConfigApplyPlan` 判定 `LlmRouter`，再对该 snapshot 执行完整 prepare→publish。
- coordinator 通过 crate-private 的 `FnOnce(&mut AppConfig) -> anyhow::Result<()>` 接收模型 mutation。
  `commands::model` 仍拥有 selector 解析、slot mutation、web-search capability 校验及其错误文本；
  `config_runtime` 不依赖 `commands`。这是 app composition root 向 command mutation port 的单向调用，
  不增加跨 crate 或反向模块依赖。
- `switch_model`、`set_reasoning_effort` 与 `set_web_search` 仍在 apply 成功后调用原事件 helper；无变化时不
  prepare/publish Router，但命令成功后仍按原路径发送 `llm:config_changed` 空 payload。
- settings 继续独立使用共享 gate、`RuntimeConfigApplyPlan` 和分阶段 prepare/publish。Security、MCP、
  input pipeline、shell、context、session、tool settings、skills、logging 与 hotkey 的副作用顺序及其半失败
  语义不并入模型 helper。

## 必须保持的不变量

- 配置先 durable edit，再从该次提交返回的 immutable snapshot prepare；不得为 apply 重新读取配置。
- gate 覆盖提交及完整 Router 应用，settings 与多个 model operation 不交错。
- Router 和媒体客户端全部 prepare 成功后才 publish；prepare 失败时 Agent/Tools 不接收新 generation。
- no-op 不触发 Router rebuild；prepare 失败时已持久化配置保留，live Router/媒体 runtime 保持旧 generation。
- 模型 ID 优先于 `RequestKind` selector；web-search mode、provider capability 校验和错误文本不变。
- 命令签名、Router wire contract、事件名称/payload/顺序与 schema 均不变；事件仍在成功 apply 后由各命令发出。

## 替代方案

- 让 coordinator 接收 model ID、`RequestKind` 或 `ModelConfig`：会把 command 选择/验证知识拉入配置协调层，拒绝。
- 把 settings 的全部副作用强塞进 model helper：会抹平安全/MCP/hotkey/logging 的阶段依赖与既有半失败语义，拒绝。
- 继续由命令串接 `edit`、plan 与 Router apply：重复完整流程，容易绕过单一提交/应用 owner，拒绝。

## 影响与验证

model commands 只提供 mutation/validation 闭包并保留事件 emit；gate、durable edit、Router target 判断和完整
prepare→publish 路径由 coordinator 拥有。closure 方向为 `commands::model` → `RuntimeConfigCoordinator`，
协调器不引用命令层类型。settings 的阶段编排和半失败处理仍待单独收口，不属于本 ADR 的完成范围。

回归覆盖两个 model operation 的 edit/apply 不重叠、no-op 不重建 Router、prepare 失败不 publish，以及模型
selector、web-search capability 与错误文本。验证命令：
`cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary --lib`、
`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、
`cargo test --workspace --locked`、`git diff --cached --check`。

不涉及 schema、配置格式、IPC 或用户数据，不需要重置。回滚本切片即可恢复 model command 直接编排
`ConfigService::edit` 和 Router apply 的路径。
