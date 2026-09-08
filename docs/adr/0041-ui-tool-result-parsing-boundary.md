# ADR 0041：UI 工具结果解析边界

> 本文记录 2026-08-30 的拆分决定；named export 的后续清理见 [ADR 0101](0101-tool-contract-and-result-renderer-audit.md)。

## 背景

工具结果 renderer 已按 kind 和 tool name 注册，但 JSON 解析、空内容处理和
custom shape 分类仍位于 `ToolResultCard.svelte` 的 module script 中，使公共卡片
继续承担结果协议判断。

## 决定

- 新增 `ui/src/lib/toolResultParsing.ts`，集中处理工具结果 JSON 解码、shell/
  notify/raw/generic/custom 分类和 custom shape 判断。
- `ToolResultCard.svelte` 的组件实例直接依赖解析模块；当时保留的原路径
  re-export 作为拆分过渡，后续不再作为 UI 内部 API。
- `toolResultRenderers.ts` 继续负责从已分类结果选择 renderer；不把 tool-specific
  展示逻辑重新放回解析模块。
- 保持空 shell、通知前缀、无效 JSON、JSON primitive/array 和所有已登记 custom
  tool 的分类语义，不改变 IPC、持久化或工具输出协议。

## 替代方案

- 保留解析逻辑在卡片 module script：会让公共壳继续承担协议分类，拒绝。
- 让 renderer registry 负责 JSON 解码和 shape 判断：会混合选择策略与解析职责，
  拒绝。
- 直接移除 `ToolResultCard` named exports：会扩大内部 API 变更范围，拒绝。

## 影响

这是 UI 内部职责拆分。解析结果和 renderer 选择保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 `ToolResultCard` 解析与渲染测试覆盖兼容 re-export 和各类结果分类。

## 回滚与重置

代码回滚时删除 `toolResultParsing.ts`，恢复 `ToolResultCard` module script 中的
解析实现并移除实例 import；本次不改变持久化数据或配置，不需要用户重置。
