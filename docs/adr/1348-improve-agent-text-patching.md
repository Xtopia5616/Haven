# ADR 1348：强化 Agent 文本 patch

## 状态

Accepted — 2026-10-21

## 背景

合并后的 `files.patch` 已能按精确字符串批量替换，但只按可被完整读取的文件上限加载；模型还需自行管理多个命中，无法表达上下文 diff。省略 `expected_hash` 时，写入也没有保护 patch 读取后发生的并发修改。完整文件覆盖同样允许不带版本条件。

## 决定

- `files.patch` 读取上限独立于完整文件读取上限，新增 `context_limits.file_max_patch_bytes`，默认 8 MiB，范围 1–64 MiB。patch 的输出仍受 `file_write_max_bytes` 限制，模型输入受 256 KiB 限制。
- 精确替换默认要求唯一命中；`replace_all: true` 显式替换所有命中，最多 100,000 处，`expected_matches` 可继续对确切命中数做断言。失败时返回最多三处候选行与上下文，提示重新读取并收窄匹配。
- `files.patch` 接受 `edits` 或单文件 unified diff，要求二选一。可以有多个 hunk，但拒绝多文件、创建、删除、重命名、复制和二进制 diff。输入 `path` 是唯一写入目标，diff header 路径不用于文件访问。使用 `diffy` 解析和应用上下文 hunk，避免自行实现不完整的 diff parser。
- patch 开始时计算被读取字节的 SHA-256；若调用方提供 `expected_hash`，它作为读取前的额外版本条件，否则写入前使用读取快照的哈希拒绝并发修改。
- `files.write` 必须提供 `expected_hash`：已有文件使用 `files.inspect` 返回的 SHA-256，新文件仅在 inspect 确认不存在时使用 `missing`。运行时再次检查，schema 不作为唯一保护。
- 新配置由 `ContextLimitsConfig` 默认值承接旧配置，不改变数据库、事件或历史 transcript；不需要数据库重置。

## 替代方案

- 将 patch 上限继续绑定到完整读取上限：拒绝；定向 patch 只需在本地读取源文件，不会把整份文件注入模型上下文。
- 默认替换全部重复命中：拒绝；重复内容可能代表多个不同语义位置，必须显式确认。
- 自行解析 unified diff：拒绝；hunk 行号偏移、上下文匹配和多文件边界容易出现安全与正确性缺口，采用专用库并在本层强制单文件、仅修改约束。
- 让 patch 仅依赖调用方哈希：拒绝；省略哈希时仍必须保护 patch 自己读取的快照。

## 影响与验证

设置页增加独立 patch 文件上限；Rust 配置、工具 JSON Schema、Agent 提示和前端设置契约同步更新。回归覆盖默认配置、上限校验、`replace_all` 与命中容量上限、候选上下文、单文件多 hunk、偏移应用、多文件/创建拒绝、源文件超出完整读取上限仍能 patch、patch 读取上限和写入并发保护。

## 回滚 / 重置

回滚代码与本文档即可恢复旧 patch 行为。旧配置缺失的新字段自动使用 8 MiB 默认值；无需删除用户配置、数据库或缓存。
