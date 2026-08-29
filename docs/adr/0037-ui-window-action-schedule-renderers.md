# ADR 0037：UI window/action/schedule renderer 边界

## 背景

窗口列表、后台任务结果和定时任务结果是三个独立的工具类型，分别拥有窗口
列表、任务状态和定时任务列表的结果形状。它们没有共享本地交互，却共同占用
`ToolResultCard` 的复杂条件模板。

## 决定

- 新增 `ToolWindowResult.svelte`、`ToolActionResult.svelte` 和
  `ToolScheduleResult.svelte`，分别承载三个工具类型的 body renderer。
- `toolResultRenderers.ts` 按 `custom + toolName` 选择对应组件；公共卡片继续
  负责折叠、参数、复制菜单和 hint。
- 保持窗口空态、任务状态 badge、后台结果回灌、定时任务列表和原有中文文案；
  不改变相关 IPC DTO、事件或后端任务语义。

## 替代方案

- 继续在 `ToolResultCard` 添加条件分支：会扩大单体卡片，拒绝。
- 把三类结果合成一个 action renderer：会丢失工具类型边界和各自的结果 shape，拒绝。
- 在本片修改窗口/任务数据契约：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部 renderer 拆分。窗口、后台任务和定时任务结果的展示保持不变，不需要
数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 ToolResultCard window/actions/schedule 测试覆盖新注册 renderer。

## 回滚与重置

代码回滚时删除三个 renderer 组件，移除 registry 映射并恢复 `ToolResultCard` 对应
分支；本次不改变持久化数据或配置，不需要用户重置。
