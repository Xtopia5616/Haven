# ADR 0969：派生集合使用明确的实体复数名称

## 状态

已接受并实现（2026-10-11）。

## 背景

`docs/naming.md` 已要求派生集合按实体命名，但仍有生产局部变量、参数和结果字段以裸 `selected`、`filtered`、`remaining`、`kept` 表示集合。前端主要 store 模块也有 `contextMenu.ts` 这种文件名未标识其 store 角色的情况。含糊名称使调用点难以区分集合结果与单项选择，也不利于跨层统一术语。

## 决定

- 派生集合使用实体复数名，例如 `selected_tools`、`filtered_embeddings`、`remaining_envelopes`、`kept_server_names`；单值或数量仍按其真实含义命名，不机械添加复数后缀。
- 将 UI context-menu store 模块及其导入统一命名为 `contextMenuStore.ts`。
- 对由 UI 用户选择的 ask option 字符串集合使用 `selectedOptions`；这些值是回答文本，不是独立 ID。

## 影响

这些变更只统一内部符号和模块名，不改变数据、算法或 IPC shape。组件及测试的导入路径已同步；没有运行时兼容 alias。

## 验证

- 对 Rust 和 UI 生产代码检索裸形容词集合命名，确认剩余 `remaining` / `selected` 等命中为数值、单值或 CSS 状态。
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `corepack pnpm run check`
- `corepack pnpm run test:run`

## 回滚

恢复原局部名称和 `contextMenu.ts` 路径，并同步对应调用点；无数据或协议迁移。
