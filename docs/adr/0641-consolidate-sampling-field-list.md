# ADR 0641：收敛 sampling 字段清单 owner

## 状态

已采纳并实施。

## 背景

`ui/src/lib/apiStyle.ts` 为设置提示维护 `SamplingField` literal union 和 `SAMPLING_FIELDS` 运行时数组，两者列出同样的九个字段。仓库没有消费该类型或数组的其它模块；`supportsSamplingField` 接受任意字符串，并针对未知或不受当前 wire style 支持的字段返回 `false`。union 没有收窄消费者契约的作用。注释还引用了仓库中不存在的 `haven_common::config::supports_sampling_field`，使这份 UI 展示清单看起来像一个并不存在的后端契约。

## 决定

1. `SAMPLING_FIELDS` 是 settings hint 遍历的唯一字段清单，设为模块私有 `as const` 数组。
2. 删除无消费者的 `SamplingField` 导出及其重复 literal union；`supportsSamplingField` 的 field 参数明确接受 `string`，继续对未知值返回 `false`。
3. 把注释改为描述该集合真实用途：构造每种 wire style 的设置提示，不宣称它是 Rust/generated enum。

## 替代方案

- 保留 union 和数组并要求人工同步：拒绝。它们表达同一词汇，存在漂移风险。
- 把未知字段从函数输入类型中排除：拒绝。运行时函数本来就通过 `false` 处理任意不支持值，收窄类型会与行为不符。
- 保留旧导出以照顾潜在外部调用：拒绝。仓库内没有消费者，且本次重构不保留未使用的旧兼容入口。

## 影响与验证

九个展示字段、wire-style 支持判断和提示文案不变；仅删除未消费的模块导出和重复类型来源。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复 `SamplingField` union 与导出并同步撤回命名规范、路线图和索引记录；无 IPC、配置或持久化影响。
