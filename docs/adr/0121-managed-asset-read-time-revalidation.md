# ADR 0121：受管资产读取时再次验证路径

日期：2026-09-10
状态：已采纳（受管文件竞态补强切片）
关联：[ADR 0116：媒体边界加固](0116-media-boundary-hardening.md)、
[ADR 0118：活动会话资产租约与上传暂存清理](0118-active-session-asset-leases.md)

## 背景

`ManagedAssetRegistry` 注册时会检查 canonical path、符号链接和 Windows
reparse point，但注册和实际 `files(asset_id)` 读取之间存在时间窗口。文件或
父目录被替换后，单靠注册时的结论不足以证明当前路径仍属于受管根目录。

## 决定

- 每个 registry 条目保存其受管根目录；`files` 在任何 managed read/summary
  开始前重新比较 registry 条目并重跑根目录、canonical、普通文件和 reparse
  检查。发生变化或检查失败时 fail-closed，不向文件读取函数传入路径。
- 该检查不改变普通用户路径的既有权限边界；`asset_id` 仍只允许 read/summary，
  host path 仍不会出现在 provider-facing 结果中。
- Windows 原生 no-follow handle 仍是未来进一步缩小最后一个 check-to-open
  窗口的增强方向；当前切片先确保每次 managed read 都不复用过期的注册证明。

## 验证与回滚

单测覆盖已注册文件在读取前消失时的 fail-closed 行为，以及既有注册、租约和
清理回归。回滚只需移除 read-time revalidate 与条目的根目录元数据，无数据库
schema 迁移。
