# ADR 0073：状态颜色语义标准

## 背景

状态圆点和通知目前都表达成功、警告、错误和信息，但之前各自直接引用
不同的颜色 token；通知左侧边条还固定使用灰色，导致同一状态在不同位置
看起来不一致，也不利于后续扩展到其他状态区域。

## 决定

使用 `ui/src/lib/statusColors.ts` 作为 UI 状态颜色的唯一语义映射：

| 语义 | 圆点/边条 | 背景 | 前景 |
|---|---|---|---|
| `success` | `success` | `success-container` | `on-success-container` |
| `warning` | `warning` | `warning-container` | `on-warning-container` |
| `error` | `error` | `error-container` | `on-error-container` |
| `info` | `primary` | 当前 primary 轻混合背景 | `on-secondary-container` |
| `tool` | `tertiary` | `tertiary-container` | `on-tertiary-container` |
| `neutral` | `outline` | `surface-container-high` | `on-surface-variant` |

这些值全部引用现有 M3 light/dark token，不在组件中硬编码颜色。`StatusDot`
只使用“圆点/边条”颜色；`NotificationToast` 同时使用三列 token，左侧边条
与状态色一致。现有 `primary`、`tertiary`、`outline` 输入分别兼容为
`info`、`tool`、`neutral`。

## 影响与验证

未改变通知事件或状态契约，只统一 UI 表现。验证包括状态映射单元测试、
`StatusDot` 测试、UI 类型检查，以及 app-binary 托盘图标测试。

## 回滚

回退本 ADR 对应提交即可恢复各组件原有颜色引用，不涉及数据或配置迁移。
