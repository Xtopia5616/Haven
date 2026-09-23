# ADR 0213：OperationSpec 是运行时策略与 manifest 的唯一来源

- 状态：accepted
- 日期：2026-09-23
- 范围：`haven-tools`
- 关联：收紧 [ADR 0211](0211-operation-registry-and-platform-snapshot.md) 和 [ADR 0212](0212-process-services-off-tools-facade.md) 里「OperationSpec 只覆盖 builtin view」的范围。不新增 `AppRuntime`，不把确认放进执行器，不改 `get_tools` 的字段形状。

## 背景

builtin operation view 的 manifest 已经从 `OperationSpec` 投影，但运行时仍按 effect 重算 confirmation，而 `OperationContract.read_only` 又把 manifest confirmation 放宽成 `none`。`files.list` 等操作因此出现运行时 `security_policy`、目录 `none`。`shell`、`http`、`tool_catalog`、`load_skill`、`load_mcp`、`ask`、`notify` 以及 MCP/Skill adapter 仍各自实现 policy 或 manifest。

## 决定

1. `OperationSpec` 是一次调用的 `policy_for` 和目录上界 `catalog_policy` 的唯一记录。`ToolManifest`、`ToolPolicy`、`ToolPresentation` 只由 `project_tool_manifest` 从 `catalog_policy` 投影。`OperationPolicy` 仍是授权引擎消费的 typed policy。
2. 确认规则是 `confirmation_for`：`Critical` 为 `Required`；effect 为 `ReadOnly` 或风险为 `Safe` 为 `None`；否则 `SecurityPolicy`。这里的 read-only 是 `OperationEffect::ReadOnly`。`OperationContract.read_only` 只决定幂等，不再豁免确认。
3. 没有 spec 的聚合工具和测试 mock 继续走 `synthesize_operation_policy`。这条路径的确认使用 `confirmation_for(risk, false)`，保持原有风险表，不因 effect 放宽。
4. 手写 spec 的 `files.read`、`files.outline`、`files.summary`、`files.search`、`system.info` 保持原 effect；`system.info` 的 scope 仍是 `Global`。`files.search` 空输入是 `Low` + `None`，目录上界是 `Medium` + `security_policy`。`mode=content` 只把该次风险升到 `Medium`，不改确认。
5. 视图的 risk、concurrency、scope 在注册时从聚合工具冻结。`media` 无 `asset_id` 时是 `Resource("media:unknown")`；音频操作是 `Resource("media:audio-device")`。`agent.inbox` 的幂等以 contract 的 `Idempotent` 为准，不用聚合实现的 `NonIdempotent`。
6. `shell`、`http`、`tool_catalog`、`load_skill`、`load_mcp` 使用 `root_operation_spec`。`http` 的 `HttpVerb` 只改幂等和并发：空输入/`GET` 为 `Idempotent` + `SharedResource("http")`，`POST` 为 `NonIdempotent` + `Resource("http")`。`ask`/`notify` 的 spec 记录空输入策略（幂等 `Unknown`）；解析成功后覆盖 typed metadata 的 risk、idempotency、scope、concurrency。`tool_def` 的 retry safety 仍用 `default_metadata`，spec 描述使用 adapter 文案。
7. MCP 与 Skill 的 spec 使用 `OperationPolicy::external`。非 Critical 的外部能力，包括 Safe，仍是 `SecurityPolicy`。
8. 不新增 `AppRuntime`。组合根仍是 `ApplicationRuntime`，热更新仍替换 `PlatformRuntime`。交互式确认仍在 `execute_tool` 之前。

## 替代方案

- 继续让 manifest 使用 `read_only` 豁免：目录会比运行时更松，模型可见策略和授权不一致。
- 让 `files.search` 的 content 模式在运行时也升到 `security_policy`：会改变已经接受的运行时授权。
- 把 `ask` 空输入的幂等改成 `NonIdempotent`：授权可以发生在参数校验之前，空输入必须保持 `Unknown`。

## 影响

- `files.list`、`files.hash`、`files.inspect`、`files.stat`、`checklist.list`、`clipboard.read`、`clipboard.history`、`window.list`、`media.inspect` 的 manifest confirmation 从 `none` 改为 `security_policy`，与运行时一致。运行时授权不放宽。
- `get_tools` 的字段名不变。上述操作的 confirmation 字符串变紧。
- 不改变持久化 schema、权限 key 或 provider schema。

## 验证

- `cargo fmt -p haven-tools`
- `cargo clippy --locked -p haven-tools -- -D warnings`
- `cargo test --locked -p haven-tools --lib`

## 回滚与重置

不改变持久化或配置 schema。回滚代码即可，无需用户数据重置。
