# ADR 0038：UI http/clipboard/web_search renderer 边界

## 背景

HTTP 响应、剪贴板操作和 web search 结果是三个独立的工具结果类型，分别拥有
状态码/正文、剪贴板历史和外部引用列表的展示形状。它们没有共享局部交互，
却继续占用 `ToolResultCard` 的条件模板。

## 决定

- 新增 `ToolHttpResult.svelte`、`ToolClipboardResult.svelte` 和
  `ToolWebSearchResult.svelte`，分别承载三个工具类型的 body renderer。
- `toolResultRenderers.ts` 按 `custom + toolName` 选择对应组件；公共卡片继续
  负责折叠、参数、复制菜单和 hint。
- 保持 HTTP 状态/截断提示、剪贴板写入/历史/读取空态以及 web search 查询、
  引用和 snippet 的原有中文文案与链接行为；不改变 IPC DTO、搜索契约或后端
  工具语义。

## 替代方案

- 继续在 `ToolResultCard` 添加条件分支：会扩大单体卡片，拒绝。
- 把三类结果合成一个通用文本 renderer：会丢失结果 shape 和引用行为，拒绝。
- 在本片修改 HTTP、剪贴板或搜索数据契约：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部 renderer 拆分。三类工具结果的展示保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

## 回滚与重置

代码回滚时删除三个 renderer 组件，移除 registry 映射并恢复 `ToolResultCard`
对应分支；本次不改变持久化数据或配置，不需要用户重置。
