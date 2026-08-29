# ADR 0036：UI process 工具结果 renderer 边界

## 背景

`process` 工具结果包含进程过滤、分页式显示全部、CPU/内存比例和状态本地化。
这些 state 与公共工具卡片和其他工具结果无关，继续留在 `ToolResultCard` 会让
一个组件同时负责多套交互。

## 决定

- 新增 `ToolProcessResult.svelte`，集中拥有进程筛选、显示上限切换、内存比例、
  状态文案/样式映射和表格 renderer。
- `toolResultRenderers.ts` 在规范化结果 kind 为 `custom` 且工具名为 `process` 时
  选择 process renderer；公共卡片仍负责折叠、参数、复制菜单和结果 hint。
- 保持 50 行默认上限、筛选优先级、CPU clamp、内存最大值归一化、状态映射和原有
  中文文案；不改变 process IPC DTO 或后端采集语义。

## 替代方案

- 继续在 `ToolResultCard` 内维护 process state：会耦合公共壳与长表格交互，拒绝。
- 将筛选提升到聊天页：会使页面承担工具结果的局部状态，拒绝。
- 在本片修改进程 DTO 或采样策略：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部 renderer 拆分。进程工具事件、结果 JSON、筛选展示和视觉语义保持不变，
不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 ToolResultCard process、筛选、上限和状态展示测试覆盖新注册 renderer。

## 回滚与重置

代码回滚时删除 `ToolProcessResult.svelte`，移除 registry 中的 process 选择并恢复
`ToolResultCard` process 分支；本次不改变持久化数据或配置，不需要用户重置。
