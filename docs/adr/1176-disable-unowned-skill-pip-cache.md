# ADR 1176：禁用无 owner 的 Skill pip 用户缓存

## 状态

已采纳并实施。

## 背景

Skill 的 Python 依赖按 `requirements.txt` 指纹安装到 `venv_root/<skill-name>`。pip 默认还会把下载与构建缓存写入用户配置目录；这些副本不属于某个 Skill venv，也没有 Haven 的历史引用、保留时间或容量限制。requirements 未改变时 Haven 不会再次调用 pip，因此常规执行不依赖该缓存。

## 决定

- pip 安装命令显式使用 `--no-cache-dir`。依赖仍安装在各自的 venv，只有额外的 pip 下载/构建缓存被关闭。
- 重建 venv 或 requirements 变化时允许重新下载依赖，不扫描或删除用户已有 pip 缓存。
- 未来若 profile 数据证明共享缓存有明显收益，应由 Haven 自己提供专属根目录、容量上限、清理 owner 和并发语义后再启用。

## 替代方案

- 继续使用 pip 默认用户缓存：拒绝，该目录脱离 Haven 生命周期，缓存规模和删除时机不由 Haven 控制。
- 现在新增 Haven 共享缓存：暂缓，当前没有测量到的重复安装成本足以承担新增的可写根、配额和 GC 状态。

## 影响与迁移

只影响后续 venv 创建及 requirements 更新时的 pip 安装速度；已有用户 pip 缓存保留在原位置，但新安装不会读取或继续扩充它。venv、配置和数据库格式不变，无需重置。

## 验证

- 命令契约测试确认 pip 参数包含 `--no-cache-dir`，requirements 快照路径与工作目录行为保持不变。
- 运行 workspace 格式检查，以及 Skills crate 全量测试和严格 Clippy。
