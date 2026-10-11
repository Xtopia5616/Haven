# 0947：在 Memory command response 边界校验实体 ID

## 状态

已接受并实现（2026-10-11）。

## 背景

Memory Tauri commands 使用生成的静态 TypeScript DTO，但 renderer 没有运行时校验返回的 ID。facts 和 recall 结果进入 Memory views；source reference 和 recall `entity_id` 用作跨实体引用或 UI key。Haven ID 规范中 fact 是 `fact-*`，transcript/source message 与 episode 共用 `msg-*` 空间。

## 决定

- `list_facts` 与 `add_fact` response 必须有规范 `fact-*`；非空 `source_ref` 必须有规范 `msg-*`。
- `recall_memory` response 根据请求 kind 校验 `entity_id`：`fact` 接受 `fact-*`，`episode` 接受 `msg-*`；省略或 null kind 按 Rust command 默认值 `fact` 处理。
- Memory command adapter 在返回结果给页面前解析 response；非法数组、实体 ID 或 source reference 会抛出边界错误。
- 不对正文、embedding model 和其它扩展字段做额外限制；外部 provider 值保持原样。

## 影响

合法 Rust response 的 wire shape 不变。错误前缀或非规范 UUID32 的 Memory row 不会进入 renderer。数据库、IPC DTO 和用户数据均未改变，无需重置数据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 回滚

若 Memory 新增 entity kind 或新的跨实体来源字段，应先由 Rust contract 与 ID 规范定义其 identity 类型，再扩展此解析器和测试；不要直接绕过 response 边界校验。
