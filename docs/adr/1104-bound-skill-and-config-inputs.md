# ADR 1104：限制 Skill 与配置输入资源并校验路径

## 状态

已接受（2026-10-13）

## 背景

Skill 环境准备会读取并交给 pip 一个由磁盘提供的 `requirements.txt`；目录扫描会枚举用户 Skill 根目录并在异步调用中同步访问磁盘；配置加载会将整个 TOML 文件读入内存，并在解析失败时再复制原文件。上述路径缺少总输入上限，且目录扫描可能长时间占用 Tokio worker 或阻塞 catalog 查询。

## 决定

- `requirements.txt` 限制为 1 MiB。通过 Tokio 文件 API 异步打开、检查和有界读取；对超过上限的输入明确报错，不开始安装或写 fingerprint。解析 skill root 与 requirements 文件的 canonical path，拒绝落在 skill root 之外的符号链接/重解析点；fingerprint 和 pip install 使用同一个已校验 canonical path。
- Skill 根目录一次最多处理 1024 个条目；超限使扫描失败，不发布部分目录。完整扫描和 watcher fingerprint 扫描均通过 `spawn_blocking` 运行。独立的刷新互斥量串行化配置、启用状态和扫描操作；磁盘扫描期间释放 catalog `RwLock`，只有完整成功后才更新配置和 catalog。失败保留旧快照与 catalog version。
- `config.toml` 限制为 4 MiB。读取最多保留上限加一字节，以检测 metadata 检查后的文件增长。超限返回错误，不备份原文件，也不返回默认配置；原文件保持不变。解析或语义无效但未超过上限的配置仍备份本次已读取的有界字节并保留现有默认回退行为，避免在备份阶段重新读取已变化的文件。

## 影响

超过新上限的 Skill requirements 和配置文件需要由用户缩小后重试。目录超过 1024 个条目时，catalog 不会被部分结果覆盖。配置 schema、Skill manifest schema、持久数据和既有 GET/安装语义不变；无需数据库重置或格式迁移。

## 验证

定向回归覆盖 oversized requirements 不触发安装、不写 fingerprint；requirements 外部 symlink 被拒绝且 pip 收到 canonical path；取消 bounded async read 会释放 reader；超过目录上限的 fingerprint/目录扫描保留原 catalog；超限配置不生成 backup 且原文件不变。验证结果：`cargo test --locked -p haven-skills`（44 项通过）、`cargo test --locked -p haven-common config::loader`（47 项通过）、`cargo clippy --locked -p haven-skills --all-targets -- -D warnings`、`cargo clippy --locked -p haven-common --lib -- -D warnings` 与本切片 Rust 文件 `rustfmt --check` 通过。Common `--all-targets` Clippy 仍被未改动的 `config/validation.rs` 三处 `type_complexity` warning 阻止（行 992、1166、1281）。

## 回滚

回滚时必须保留有界读取/扫描、canonical containment、扫描不持有 catalog 写锁和失败不发布部分快照这些不变量；不能只删除上限或恢复无界的同步读取。若将来提高限制，应保留明确的硬上限及相应超限回归。
