# ADR 0039：UI file_search/files renderer 边界

## 背景

`file_search` 与带 `results` 数组的 `files` 结果共享文件路径、行号和 snippet
列表展示。它们仍由 `ToolResultCard` 的条件模板直接渲染，使搜索结果样式与公共
卡片壳耦合。

## 决定

- 新增 `ToolFileSearchResult.svelte`，承载 `file_search` 以及带搜索结果的
  `files` body renderer。
- `toolResultRenderers.ts` 按 `custom + toolName` 注册 `file_search`；仅当
  `files` 数据包含 `results` 数组时选择同一 renderer，其他 `files` 结果保持既有
  generic/custom 分类行为。
- 将文件路径外部引用、行号、snippet、结果计数和空态样式迁移到该组件；保持
  `ExternalRef` 的复制/打开行为、原有中文文案、排序和 key 语义，不改变 IPC DTO
  或后端搜索语义。

## 替代方案

- 继续在 `ToolResultCard` 添加搜索条件分支：会扩大单体卡片，拒绝。
- 让所有 `files` 结果无条件使用搜索 renderer：会误处理文件读写/目录结果，拒绝。
- 在本片修改文件搜索数据契约：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部 renderer 拆分。文件搜索结果的展示和链接行为保持不变，不需要数据
或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 `ToolResultCard` 文件搜索测试覆盖 registry 接管后的结果、空态和 hint。

## 回滚与重置

代码回滚时删除搜索 renderer 组件，移除 `file_search/files` registry 映射并恢复
`ToolResultCard` 搜索分支；本次不改变持久化数据或配置，不需要用户重置。
