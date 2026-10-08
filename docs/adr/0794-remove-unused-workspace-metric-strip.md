# ADR 0794：移除未使用的工作区指标组件

## 状态

已采纳并实施（2026-10-08）。

## 背景

`WorkspaceMetricStrip.svelte` 定义了指标数组、开放文案和值类型以及专用样式，但全仓没有任何 import 或渲染调用；组件内部 CSS 也没有跨组件消费者。该 view 和 Props surface 没有实际 owner 或用户可见入口。

## 决定

删除未消费的 `WorkspaceMetricStrip` 组件及其局部 Props/type/style。未来出现具体指标展示消费者时，再按该页面的实际数据 owner 建立组件。

## 影响与回滚

仓库内没有调用方，因此不改变现有界面、IPC 或持久化。回滚时可从此 ADR 的前一提交恢复组件文件。

## 验收

全仓搜索确认组件名与专用 class 无外部引用；UI 类型检查与全量测试通过。无 IPC、配置或持久化变更。
