# ADR 0035：UI system 工具结果 renderer 边界

## 背景

`system` 工具结果包含 CPU、内存、磁盘、电池、显示器和环境变量等互不相同的
展示形状，其中环境变量还有筛选与复制交互。它不依赖文件、进程或任务结果的
状态，却继续占用 `ToolResultCard` 的复杂模板和本地 state。

## 决定

- 新增 `ToolSystemResult.svelte`，集中渲染 system 快照、显示器、环境变量筛选/复制、
  电池和电源状态。
- `toolResultRenderers.ts` 在规范化结果 kind 为 `custom` 且工具名为 `system` 时选择
  system renderer；公共卡片、context menu 和复杂结果的 hint 仍由 `ToolResultCard` 管理。
- 保持指标进度条 clamp、字节/运行时长格式化、环境变量筛选、剪贴板失败静默处理及
  原有中文文案；不改变 system IPC DTO 或安全语义。

## 替代方案

- 继续把 system 分支放在 `ToolResultCard`：会让页面卡片继续聚合无关本地 state，拒绝。
- 把环境变量筛选状态提升到父卡片：会扩大公共壳的职责，拒绝。
- 在本片同步修改 system 返回 DTO：跨越 IPC 边界且没有必要，暂不采用。

## 影响

这是 UI 内部 renderer 拆分。system 工具事件、结果 JSON、复制行为和视觉语义保持不变，
不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 ToolResultCard system、环境变量、指标和电源状态测试覆盖新注册 renderer。

## 回滚与重置

代码回滚时删除 `ToolSystemResult.svelte`，移除 registry 中的 system 选择并恢复
`ToolResultCard` system 分支；本次不改变持久化数据或配置，不需要用户重置。
