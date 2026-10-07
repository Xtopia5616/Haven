# ADR 0645：命名 Model discovery 的配置投影与 metadata patch

## 状态

已采纳并实施。

## 背景

`modelDiscovery.ts` 手写 `Provider` 配置字段子集，`ModelDiscoveryContext.getModels` 和 `applyDiscoveredModelMeta` 使用 `Record<string, any>`，发现完成后则以 `Record<string, unknown>[]` 把配置 patch 跨 ModelSettings 与 SettingsView 传递。实际 provider/model 来源均是设置页中的 `ProviderDraft` / `ModelDraft`。metadata helper 只更新 `context_window`、`cost_per_1k_input_tokens` 与 `cost_per_1k_output_tokens`，而 `SettingsView` 仍需按字符串键和运行时类型 guard 才能写回 baseline。

## 决定

1. `ModelDiscoveryProvider` 由 `ProviderDraft` 的 discovery 所需字段 `Pick` 派生；ModelDiscoveryContext 与 metadata mutator 使用 `ModelDraft`，删除局部宽泛 Provider shape 和 `Record<string, any>`。
2. 以 `DiscoveredModelMetadataFill` 表达 `{ id, ...metadataPatch }`，metadata patch 只允许 `ModelDraft` 的 context window 和 input/output cost 字段。
3. `ModelDiscoveryContext`、`ModelSettings` 与 `SettingsView` 共用该 fill 类型；内部静态 callback 删除对 `Array.isArray` 和数值类型的重复运行时检查，保留 undefined 不写入、null 清空的行为。

## 替代方案

- 继续传 `Record<string, unknown>`：拒绝。回填字段集合在生产路径固定，开放 map 让未知 key 穿过多个组件。
- 直接把完整 ModelDraft 作为回填内容：拒绝。baseline 只需 id 与 discovery 更新字段；全对象会误表述被修改的范围。
- 在 SettingsView 再声明同一 patch shape：拒绝。model discovery 是该更新集合的生产 owner，应向消费者提供同一结果类型。

## 影响与验证

model catalog 选择、自动补充和 overwrite 时机不变；只收窄内部输入/回填类型，并删除多余字段与宽泛类型。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复本地 Provider shape、宽泛 record 类型和 SettingsView 的 runtime guards，并同步撤回命名规范、路线图和 ADR 索引；无 IPC、持久化或外部配置影响。
