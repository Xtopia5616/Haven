# ADR 0115：受管上传资产生命周期与清理

日期：2026-09-10
状态：已采纳（阶段 3 生命周期切片）
关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)

## 背景

阶段 3 将普通附件保存到 `default_work_dir()/uploads/<batch>/`，并用
`asset_id` 让 `files` 工具在受管边界解析路径。此前注册表和文件目录只会增长，
进程重启后也没有统一的清理责任；工具本身不应因为模型请求而删除宿主文件。

## 决定

- `haven-app-binary` 是上传目录的唯一清理 owner；`haven-tools` 只持有当前进程
  的 `asset_id → host path` 注册表，不负责 TTL 和删除。清理先从持久化消息收集
  仍被引用的路径，再 prune 不在引用集合中的 registry 条目；删除后继续 prune
  无效映射。
- 每个已提交上传批次使用 `file-{uuid32}` 目录名；写入期间使用隐藏的
  `.file-{uuid32}.tmp` staging 目录，提交时原子改名。清理只接受这两种精确格式，
  并且只遍历专用 `uploads` 根目录的直接子目录；普通文件、未知目录名和非目录项跳过。
- 清理 TTL 复用 `memory.history_retention_days`。启动时执行一次，每 24 小时执行
  一次；当历史保留设置为 0 时不自动删除上传批次。
- 清理前检查目录修改时间，删除只针对超过 TTL 且没有持久化消息引用的已生成批次；
  staging 目录同样可被清理。删除失败只记录脱敏日志并继续处理其它批次。
- 资产注册仍是进程内的。resume/新会话从事件或初始附件重新注册；过期、缺失或
  进程重启后未重新注册的 `asset_id` 对模型表现为不可用，而不是回退到路径猜测。

## 安全与恢复

清理不接受模型参数，不调用通用 `files` 删除操作，也不以 renderer 提供的 id
决定删除目标。批次目录下的附件是由 host 校验和写入的受管文件；目录清理不会
触碰 `uploads` 外的路径。历史消息可能保留已过期的 `asset_id`，工具返回
`managed asset is unavailable or expired`，调用方可选择重新上传。

## 验证与回滚

单测覆盖严格批次名校验、只清理生成目录、保留非生成目录、目录缺失的幂等行为、
活动注册表引用保护和删除后的 prune。
回滚只需停止 app-binary 的启动/每日 cleanup 调度；旧上传目录仍可由人工清理，
数据库和 snapshot 无 schema 变化。
