# ADR 0845：从 Chat 提交中投影掉文件预览大小

## 状态

Accepted — 2026-10-10

## 背景

Composer 用 `PendingChatFileAttachment.size` 显示待发送文件大小。`handleSubmit` 原样转交整个待发送对象，导致仅供预览的 `size` 穿过页面、session controller 和 submit coordinator，最终进入 `process_transcript` invoke 参数。Rust `MessageAttachmentInput`、生成的前端输入契约以及持久附件都没有这个字段；文件大小不参与服务端处理，也不应成为跨层输入契约的一部分。

## 决定

- Composer 的提交 callback 声明 `ChatFileAttachment[]`，不声明本地 pending view。
- 在 `handleSubmit` 边界从每个 pending file 中显式投影 `media_type`、`data` 与 `filename`；`size` 留在 `PendingChatFileAttachment` 并继续驱动预览。
- 测试验证 5 B 预览仍可见，同时提交对象不包含 `size`；另一个并发读取/名额测试也断言 canonical payload。
- 不为这个单向投影引入另一份共享类型或额外 adapter。

## 替代方案

- 继续透传并依赖 Serde 忽略未知字段：拒绝。会让调用链长期携带不属于契约的字段，并使 UI request shape 与 generated contract 分叉。
- 把 `size` 加入 generated / Rust 附件输入：拒绝。没有消费方或行为需要它；只为 UI 预览扩展 IPC 只会扩大契约。
- 将 `PendingChatFileAttachment` 改为只存原始 `File` 并在模板临时读取大小：拒绝。没有必要改变已稳定的本地预览状态形状。

## 影响与验证

只改变 Composer 到宿主 callback 的 UI 内部提交对象投影。命令名、Rust DTO、JSON 字段、媒体处理、附件持久化和 transcript 行为不变；无持久化数据影响，无需重置。验证通过：Svelte check（0 errors / 0 warnings）、全量 UI 测试（131 个文件、1028 项）、生产构建、Composer 源码与本 ADR 的 Prettier、ADR 索引（828 条记录）和 staged diff 空白检查。`docs/naming.md` 与本路线图的全文件 Prettier 在 HEAD 基线即失败，差异不由本切片引入；未格式化无关历史内容。

## 回滚

如将来服务端确需文件大小，应先定义其业务用途并显式扩展 `MessageAttachmentInput`；不恢复隐式透传的 preview 字段。
