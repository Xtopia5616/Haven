# ADR 0846：将 Chat 提交回调直接接到所属 controller

## 状态

Accepted — 2026-10-10

## 背景

`+page.svelte` 同时定义 `submitMessage` 与 `handleInputSubmit` 两个转发函数。前者仅调用 `ChatSessionController.submitMessage`；后者只把 Composer payload 重建为同形对象，再调用 `chatAskInteraction.handleInputSubmit`。route 没有在这两处增加状态、校验、错误处理或副作用，因此额外函数名遮住了实际 owner，也让 callback 调用图多一跳。

## 决定

- 将 `askInteraction.handleInputSubmit` 直接传给 `Composer.onsubmit`，删除 route 的 `handleInputSubmit` 与它的别名。
- AskInteraction 的 submit port 由 route 直接接到 `ChatSessionController.submitMessage`，以闭包保留 class method 的 receiver，并显式丢弃其内部处理完错误的 `Promise<void>`。
- 删除 `submitMessage` 转发函数及因其存在而只在该函数使用的附件类型导入。
- 保留 AskInteraction 的批量问答处理、auto-follow、附件转交；保留 ChatSessionController 的 session selection、提交协调和错误处理；不改 reducer、命令或 wire contract。

## 替代方案

- 保留包装函数以让 route “拥有”统一入口：拒绝。函数没有额外行为或独立策略，route 不应重新拥有别的 controller 的流程。
- 把提交逻辑并入 route：拒绝。ask batch 与 session command submission 已分别有明确 owner，合并会重新将业务流程放回页面。
- 让 Composer 直接调用 Session controller：拒绝。Composer 应只产出并回调输入 payload，不应依赖 ask/session 领域流程。

## 影响与验证

只缩短 UI 内部 callback 调用链。输入类型与顺序、ask 选择与回答组成、auto-follow、Session reducer 状态、Tauri 命令、错误通知、持久化和重启恢复行为不变；无持久化影响，无需重置。既有 `chatAskInteraction` 与 `chatSessionController` 行为测试覆盖两个 owner 的语义。验证通过：Svelte check（0 errors / 0 warnings）、全量 UI 测试（131 个文件、1028 项）、生产构建、本切片 route 与 ADR 的 Prettier、ADR 索引（829 条记录）及 staged diff 空白检查。路线图全文件 Prettier 在 HEAD 基线即失败，未格式化无关历史内容。

## 回滚

无需数据回滚。若 route 将来在此边界增加实际策略，应以该策略命名单一 callback owner，而不是恢复无行为的同义转发函数。
