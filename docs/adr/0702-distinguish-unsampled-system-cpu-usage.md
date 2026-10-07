# ADR 0702：区分系统概览中的未采样 CPU 使用率

## 状态

已采纳并实施。

## 背景

System 默认 overview 为减少固定采样等待，不调用第二次 CPU refresh，并从 `cpu` 对象中移除 `usage_pct`。`ToolSystemResult` 曾用 `Number(usage_pct ?? 0)` 渲染该值，因此默认 overview 将“未采样”显示为 `0.0%` 和空 meter，与真实采样得到的 0% 无法区分。

## 决定

- 只有 `cpu.usage_pct` 是有限数值时才显示百分比和 meter。
- 字段缺失或不是有效数值时显示“本次概览未采样”；核心数和线程数仍正常显示。
- 保留后端快速 overview 行为和原有输出 shape；需要真实 CPU 使用率的 `category=cpu` 继续采样并返回该字段。

## 替代方案

- 在后端 overview 中加入 `usage_pct: 0`：拒绝，0 会被误读为测量值，也不能表达未采样。
- 为 overview 增加采样等待：拒绝，默认 system call 会重新承担固定采样延迟。
- 隐藏整个 CPU 行：拒绝，CPU 型号、核心数和线程数仍可用。

## 影响与验证

只改变缺失/无效值时的 UI 表示；实际数值与后端 JSON shape 不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）和 `corepack pnpm run test:run`（125 files、986 tests passed）。Vitest 运行期间打印 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复缺省 0% 的 renderer 表达并删除对应回归测试；撤回本 ADR 和路线图条目。无需数据迁移。
