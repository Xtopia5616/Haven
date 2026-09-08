# ADR 0034：UI 文件工具结果 renderer 边界

> 工具名称与旧 renderer 入口的后续决定见 [ADR 0100](0100-remove-tool-name-compatibility.md)。本文保留为历史拆分记录。

## 背景

文件工具结果包含写入、编辑、复制、移动、删除、目录列表和文件读取等多种
互斥形状，并依赖 `ExternalRef` 与文件尺寸格式化。它与 shell、系统指标和
进程筛选没有共享状态，却一直占用 `ToolResultCard` 的复杂分支。

## 决定

- 新增 `ToolFileResult.svelte`，集中渲染 `toolName=file` 的文件操作与读取结果。
- `toolResultRenderers.ts` 在规范化结果 kind 为 `custom` 且工具名为 `file` 时选择
  文件 renderer；公共卡片仍由 `ToolResultCard` 管理。
- 保持操作优先级、中文文案、目录空态、ExternalRef 目标、尺寸格式化和原 CSS 视觉
  语义不变；`files` 搜索结果继续由复杂 renderer 处理。

## 替代方案

- 继续追加 `ToolResultCard` 的文件分支：会扩大单体组件，拒绝。
- 按每个文件操作再拆成独立组件：当前操作共享同一结果状态，粒度过细，暂不采用。
- 把路径展示改成普通链接：会绕过既有本地文件打开边界，拒绝。

## 影响

这是 UI 内部 renderer 拆分。工具事件、结果 JSON、IPC DTO、持久化和文件打开行为
保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 ToolResultCard 文件写入、目录、读取和路径展示测试覆盖新注册 renderer。

## 回滚与重置

代码回滚时删除 `ToolFileResult.svelte`，移除注册表中的 `file` 选择并恢复
`ToolResultCard` 文件分支；本次不改变持久化数据或配置，不需要用户重置。
