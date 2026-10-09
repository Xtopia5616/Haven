# ADR 0844：合并 Chat Composer 组件 owner

## 状态

Accepted — 2026-10-10

## 背景

路由渲染 `Composer.svelte`，但它不实现展示或局部交互，只用 `ComponentProps<typeof InputRouter>` 重建 props、通过 ref 转发 `setDraft`，随后把所有 props 和 snippets 原样交给 `InputRouter.svelte`。后者才拥有消息草稿、附件暂存/读取、录音按钮、textarea 键盘与菜单、发送/中断行为、预览和全部输入区样式。一个用户可见的 chat input 边界因此由两层组件和两个命名共同表达。

`Composer` 与 `InputRouter` 没有独立 consumer、状态或生命周期。route/controller 已经把该边界命名为 Composer；`InputRouter` 不是路由、事件 router 或跨域命令 owner。

## 决定

- 将完整的输入 props、状态、handler、模板与样式迁入 `Composer.svelte`，由该组件直接导出 `setDraft`。
- 删除仅透传的 `InputRouter.svelte`；不保留旧组件、重导出或兼容 wrapper。
- 将原 InputRouter 组件测试并入 `Composer.test.ts`，保留附件读入等待、容量、草稿隔离、菜单、按钮和 textarea 行为覆盖，并把 imperative set-draft 测试落在最终 owner 上。
- 页面、controller、错误日志来源、架构/命名文档统一称 Composer。
- 保留约 930 行的组件边界：附件、草稿、textarea 与 toolbar 共用单一 DOM、局部状态和提交生命周期；当前没有独立消费者能支撑额外组件/状态 owner。若附件处理新增第二消费者或能在无跨组件共享可变状态的情况下独立测试/替换，再评估抽取。

## 替代方案

- 保留 wrapper 与内层组件：拒绝。双层没有单独职责，增加 props 别名、ref 转发与一跳组件树。
- 把 route 改为直接使用 `InputRouter`：拒绝。会让产品/架构边界继续使用实现遗留名，同时丢失 Composer 的当前命名 owner。
- 现在按函数数量机械拆分附件、键盘和菜单组件：拒绝。它们共享同一个 textarea、附件队列与 submit state，拆分会引入额外 props/事件通道，没有独立 consumer 收益。

## 影响与验证

仅合并 UI 内部组件结构和命名，不改变 Composer props、提交 payload、命令/wire、附件处理、录音切换或视觉布局；无持久化数据影响，无需重置。验证通过：`corepack pnpm run check`（0 errors / 0 warnings）、全量 UI 测试（131 个文件、1028 项）、`corepack pnpm run build`、本切片源码/ADR 的 Prettier 检查、差异空白检查，以及覆盖 827 条记录的 ADR 索引检查。

## 回滚

如未来确有单独的路由/输入策略 owner，可重新引入明确命名的窄组件/adapter；不恢复无独立语义的 `InputRouter` 转发层。
