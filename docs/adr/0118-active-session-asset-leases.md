# ADR 0118：活动会话资产租约与上传暂存清理

日期：2026-09-10
状态：已采纳（阶段 3 生命周期补强切片）
关联：[ADR 0115：受管上传资产生命周期与清理](0115-managed-upload-lifecycle-and-cleanup.md)

## 背景

仅以消息 UI 元数据作为上传文件保护依据，会在事件已持久化但消息
投影尚未完成、崩溃恢复或长时间运行会话中留下 GC 窗口。另一方面，
`.file-{uuid32}.tmp` 是上传失败或进程崩溃后的临时状态，不应跟随历史消息保留
开关无限期存在。

## 决定

- `ManagedAssetRegistry` 为活动 session 持有进程内 asset lease。resume、已有
  会话补充输入和首次运行统一绑定 session lease；终态 session 清理时释放。GC
  在 prune registry 前保留活动 lease，因此事件/投影尚未可见时仍不会删除文件。
- 新会话在分配 session id 前使用 pending ingress lease；成功创建后转为 session
  lease，失败则释放。pending lease 同样参与 registry prune 保护，覆盖创建与
  投影之间的短窗口。
- 持久化消息引用仍是历史资产的保护来源；lease 只解决进程内活动窗口，不改变
  `history_retention_days` 对已完成上传批次的保留语义。
- 上传 staging 目录使用独立的 24 小时清理调度，启动时和每 24 小时执行一次，
  不受 `history_retention_days = 0` 控制。已提交批次仍由历史保留清理负责。

## 安全与恢复

lease 只接受已经通过 uploads 根目录校验的 host-owned asset；不会扩大
`files(asset_id)` 的路径解析能力。进程重启后内存 lease 消失，resume 从事件或消息
`ui_metadata` 重新注册；没有重新注册且不再被历史引用的资产由后续 GC 清理。
staging 清理只遍历专用 uploads 根目录的严格 `.file-{uuid32}.tmp` 目录名。

## 验证与回滚

单测覆盖活动 lease 在消息投影缺失时仍被保留、批次删除时释放 lease、删除后
prune，以及 staging 清理独立于普通批次清理。回滚只需停止 app-binary 的 lease
保护与 staging 调度改动；无数据库 schema 变化。
