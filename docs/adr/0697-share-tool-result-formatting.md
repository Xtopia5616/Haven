# ADR 0697：共用 Tool result 数值格式化

## 状态

已采纳并实施。

## 背景

`ToolFileResult`、`ToolProcessResult` 和 `ToolSystemResult` 各自实现了相同的字节大小格式化；`ToolProcessResult` 与 `ToolSystemResult` 还重复实现相同的百分比限幅。三者均将任意 renderer 数据通过 `Number` 归一，应用相同的无效值、负值、单位与边界规则，不存在不同领域语义。

## 决定

- 将字节格式化集中为 `formatByteSize`，供三个结果组件共用。
- 将百分比边界收敛为 `clampPercentage`，供系统和进程结果组件共用。
- 保留各 renderer 对自身字段、列表和展示状态的解析职责；只共享稳定的纯格式化行为。

## 替代方案

- 继续在每个 renderer 内局部复制 helper：拒绝。已有三份相同实现和两份相同限幅逻辑，之后修复边界需要同步多处。
- 把完整 Tool result JSON 解析也移动到该格式化模块：拒绝。payload shape 与 renderer 选择属于 parser/renderer owner，不是数值格式化职责。

## 影响与验证

- 字节文字、非有限/负值回退、百分比范围及组件展示不变；不影响 IPC、持久数据或配置，无需重置。
- 验证：新增纯函数边界测试；UI `check` 与 `test:run`，ADR 索引及 `git diff --check`。

## 回滚

把两个纯函数还原为组件局部实现即可；没有数据或 wire 迁移。
