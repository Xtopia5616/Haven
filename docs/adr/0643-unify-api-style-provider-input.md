# ADR 0643：统一 apiStyle 的 Provider 输入投影

## 状态

已采纳并实施。

## 背景

`apiStyle.ts` 的 `providerWireStyle`、media/STT capability 判定、preset 显示与 keyless 判断分别内联声明重叠的 `api_style`、`provider`、`base_url` 字段。媒体判断还声明了不参与任何逻辑的 `name`。这些字段属于 generated `ProviderConfigInput`；UI 草稿可能在编辑过程中暂时提供 null `provider` 或 `base_url`，所以它不是 required wire `ProviderConfig` 的完整实例。

## 决定

1. 增加模块私有 `ProviderStyleInput`，以 `Pick<ProviderConfigInput, 'api_style'>` 为字段来源，并显式添加 UI 草稿需要的 nullable `provider` 与 `base_url`。
2. media/STT backend 与 preset/keyless helper 共用该投影；只读较少字段的 helper 使用 `Pick<ProviderStyleInput, ...>` 收窄参数。
3. 删除媒体 backend 输入中未消费的 `name` 字段。`providerWireStyle` 仍只根据 `api_style` 选择 wire protocol；vendor identity 不会改变空值时的 neutral protocol。

## 替代方案

- 继续在每个 helper 内联近似 shape：拒绝。字段增删需要多处同步，且会掩盖 API style / vendor identity 的边界。
- 直接接收完整 `ProviderConfig`：拒绝。设置草稿输入不是 required config instance，且需要在编辑中表达 null host/provider。
- 让所有 helper 接收整个 Provider config：拒绝。capability 展示不需要 config 中的密钥、超时或请求策略字段。

## 影响与验证

仅影响 UI 内部 helper 的静态输入契约和测试 fixture；provider 配置 wire shape、能力映射与 preset 选择行为不变。无需保留旧私有参数形状。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复各 helper 的内联参数 shape 与未使用字段，并同步撤回命名规范、路线图和 ADR 索引；无 IPC、配置或持久化影响。
