# ADR 0759：Media ToolResult 复用生成 representation 类型

## 状态

已采纳并实施。

## 背景

Common `MediaReference.representation` 和 `available_representations` 使用闭合 `MediaRepresentationKind`；`MediaResult.representation` 也使用相同可选 enum。该 Common 类型已由其他 Tauri DTO 引用并生成 UI 类型和值清单。Media builtin ToolResult 的专用 renderer props 和 nested guard 却把这些字段放宽为任意字符串/字符串数组。`MediaReference.modality` 还使用闭合 `DetectedMediaKind`，`file_kind` 由 Tools 的有限映射生成，二者的 root presentation props/guard 也曾是开放字符串。

## 决定

- `ToolMediaResult` props 直接引用 generated `MediaRepresentationKind`，不再定义局部同义 union。
- `contracts/media.ts::isMediaRepresentationKind` 复用 generated values；root/nested representation 与 available-representations array 都以它校验。
- renderer 消费的 root `modality` / `file_kind` 使用 UI presentation values，与 Common `DetectedMediaKind` 及 Tools 有限分类映射保持一致。
- `media.content` 继续以 `unknown` 交给字符串检查/JSON view；`recommended_next` 继续作为可扩展文字显示。

## 替代方案

继续使用字符串类型会使 Media renderer 与 Common enum 漂移；在 UI 再手写一份 11 值列表会复制 generated owner。收紧整个 media ToolResult envelope 则会错误限制仍属动态 JSON 的内容字段。

## 影响与验证

只收紧 UI Media renderer representation/modality props 与 guard，不改 Common producer、ToolResult wire、IPC 或持久化。测试覆盖未知 root representation、modality/file kind 与 nested list 项回退 JSON，以及合法值保留专用 renderer、任意 media content 仍保持动态；UI check 与 test:run 通过。

## 回滚

恢复 representation 字符串 props 与 guard 即可。没有 IPC、数据或持久化迁移。
