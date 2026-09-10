# ADR 0119：生成媒体生命周期与受管根目录

日期：2026-09-10
状态：已采纳（阶段 3 生成媒体补强切片）
关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0118：活动会话资产租约与上传暂存清理](0118-active-session-asset-leases.md)

## 背景

文生图结果此前直接写入 `data_dir/media`，没有统一的输出大小上限、完整性
元数据或过期清理；生成附件携带的路径又会被只接受 `uploads` 根目录的受管
registry 拒绝，导致 resume 和 `files(asset_id)` 的生命周期不一致。

## 决定

- 生成媒体写入独立的 `default_generated_media_dir()`，使用严格的
  `file-{uuid32}.{extension}` 文件名，与用户上传批次命名空间隔离。
- provider 响应体在流式读取时限制为 32 MiB；解码后的生成媒体限制为 16 MiB，
  空媒体和非图片 MIME 直接失败，失败时不留下已创建文件。
- 每个生成结果生成独立的 `asset_id`，记录字节数、SHA-256 和 RFC3339
  `expires_at`，并随消息附件持久化。受管 registry 在注册时恢复 expiry，过期
  asset 不再可由 `files(asset_id)` 解析。
- 生成媒体默认保留 7 天；app-binary 启动及每日维护时扫描专用根目录，过期或
  超过 fallback TTL 的严格生成文件删除。活动 session lease 优先于 expiry，
  但历史消息本身不延长生成媒体生命周期。

## 安全与恢复

renderer 上传边界会清除生成元数据字段；只有 gateway 生成的 host-owned 附件
才会带有这些字段。resume 会按生成根目录重新注册；进程重启后若 metadata 不
可用，以文件修改时间作为 7 天 fallback。清理只操作专用根目录的直接文件，
跳过 reparse point 和未知文件名。

## 验证与回滚

单测覆盖生成元数据、输出大小拒绝、严格文件名、expiry 与活动 lease 的交互，
以及 provider 响应体的有界读取实现。回滚只需停用生成目录维护和 metadata
扩展；无数据库 schema 迁移，旧附件字段按 serde 默认值兼容。
