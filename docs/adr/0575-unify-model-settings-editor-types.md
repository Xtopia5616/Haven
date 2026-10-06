# ADR 0575：统一 Model Settings 组件共享编辑类型

## 状态

已采纳并实施。

## 背景

`ModelSettings` 和 `ProviderDialog` 各自声明了相同的临时 Provider 表单字段，但子组件把它命名为 `ProviderDraft`，与 `settingsModelTypes.ts` 中基于生成配置契约的 `ProviderDraft` 混淆。父层和 ProviderDialog 还重复声明 provider key 检查输入，且子层类型缺少父层回调读取的 `provider`、`api_style` 字段。`OverrideField` 联合在 ModelSettings、ProviderList 和 ModelConfigCard 中复制了三份。

## 决定

1. 在 `settingsModelTypes.ts` 单一声明 `ProviderDialogForm`，只表示包含 UI `proxy_mode` 的临时编辑表单；配置草稿仍使用 `ProviderDraft`。
2. 以 `ProviderKeyCheckInput` 表达 key 状态判定回调真正消费的字段，并让父子组件共用该输入契约。
3. 以 `ModelOverrideField` 唯一声明每模型可编辑的 override 字段联合，供 owner、列表和卡片共同引用。

## 替代方案

- 保留组件内局部类型：拒绝，表单字段和 override 联合相同却分散维护，且局部 `ProviderDraft` 与配置草稿同名。
- 直接将生成的 `ProviderConfigInput` 当作表单类型：拒绝，临时表单含必填 UI 字段和 `proxy_mode`，而 wire 配置使用可选字段及 `proxy_url`。
- 将父子组件类型缩成各自只可见字段：拒绝，key 检查由父层 owner 执行，子层调用时应表达其完整输入。

## 影响与验证

- 类型移动到模型设置领域类型模块，Provider settings 父子组件、列表和卡片改为引用共享定义。
- 仅 TypeScript/Svelte 内部类型契约变化；provider 表单字段投影、credential 判定、override 值和 IPC payload 均不变。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

恢复父子组件各自的 Provider 表单/key-check 类型与三份 `OverrideField` 联合，并撤回共享类型命名规则。
