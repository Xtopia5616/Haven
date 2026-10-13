# 2026-10-13 全库术语与实现规范审计覆盖记录

## 范围与方法

本轮按 crate/UI/IPC/规范文档分域盘点生产源码和当前契约消费者。每个源文件由分域审计者逐项核对；高风险发现由主执行者返回源码、生产者/消费者和失败路径独立复核。UI/App/IPC 目录先建立文件清单，再对关键身份、生命周期、契约与安全文件逐行复核，其余文件按模式、调用图和消费者完成候选分类。该统计证明本轮范围内每个文件均已核对或分类，不表示每个 UI 文件都做了逐行深审。

## 覆盖统计

| 范围 | 清单数 | 完成核对/分类 | 未核对 |
|---|---:|---:|---:|
| Rust 生产源码（11 crates） | 268 | 268 | 0 |
| UI `src` 生产文件 | 246 | 246 | 0 |
| App binary（同时计入 Rust 268） | 37 | 37 | 0 |
| IPC 生成/漂移检查脚本 | 3 | 3 | 0 |
| 架构/契约核心文档与索引 | 9 | 9 | 0 |
| 本轮直接影响的历史 ADR（候选） | 18 | 18 | 0 |
| UI/App/IPC/文档组合清单（含 App binary） | 313 | 313 | 0 |

Rust 生产文件按 crate 分布：`haven-agent` 37、`haven-app-binary` 37、`haven-common` 5、`haven-input` 8、`haven-llm` 63、`haven-mcp` 7、`haven-memory` 9、`haven-messaging` 4、`haven-platform` 4、`haven-skills` 2、`haven-tools` 92。LLM 的 9 个 test-only 源文件、MCP 的 1 个 test-only 源文件不计入生产清单；内联 `#[cfg(test)]` 与覆盖回归仍随对应生产源码复核。UI `src` 总数 399，其中 246 个生产文件、152 个测试/规格文件和 1 个测试 setup；本轮 UI 生产覆盖数不把测试文件混作生产代码。

## 本轮修复与契约收敛

- 工具结果列表按稳定领域身份渲染；Ask、环境变量、窗口、UI Automation、Clipboard、Memory、WebSearch、Admin 与 MCP 名称身份与 guard 对齐（ADR 1141）。
- Skills renderer DTO 移除本机 root；Agent `skills_list` 继续按 ADR 0878 使用专用目录投影。外部错误正文、OS 路径和 OCR Debug URL 脱敏（ADR 1142）。
- partial checkpoint promotion 将 ingress 顺序检查、message materialization 和 checkpoint 删除并入同一事务；schema 升至 v42（ADR 1143）。
- Memory fact 去重保留 subject；Compaction 按图片/视频、纯音频、无媒体选择能力路由（ADR 1144）。
- VAD 队列设有界容量，shutdown 关闭 response receiver 后 join；后台取消由 Job Object 收回进程树（ADR 1145）。
- Skills manifest 在已打开句柄上执行 `cap + 1` 有界读取；有效条目与 invalid diagnostic 使用同一名称身份集合（ADR 1146）。
- `UnsupportedCapability` outcome 不污染 endpoint circuit；MCP catalog generation 在 cache 发布后递增；SSE parser 跨 chunk 保留 event 并按 line/event 限额处理大 chunk（ADR 1147–1149）。
- Common special-address classifier 按 IANA IPv4/IPv6 registry 覆盖特殊用途地址及可达例外（ADR 1150）。
- Messaging Envelope 注释现在列出 `receipt`，并说明持久 recipient 必须是具体 `ses-*`；`*` 仅是广播工具输入，在落盘前展开。
- 删除 UI Admin MCP `servers` renderer 分支；当前 `mcp_status` producer 返回 `Vec<McpStatusOutput>`，该旧 object shape 无当前生产者。

## 已核实的显式边界

Skills `SKILL.md` 与 `requirements.txt` 的 canonical path 检查和后续 pathname open 之间，仍保留同一用户可并发改写目录时的局部竞态。该能力不构成 Windows 权限边界；ADR 0513/0515 明确 Job Object 只负责进程树生命周期，不是文件系统沙箱。此项已检查并记录为当前本地用户信任模型下的显式例外，不计作未核对；若将来 Skills root 支持不可信共享写入者，应改用句柄锚定 containment，并让 requirements fingerprint 与 pip 消费同一稳定 snapshot。

## 验收门禁

- Rust：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 均通过。
- UI：`corepack pnpm run check`、`corepack pnpm run test:run`（153 个测试文件 / 1281 项通过）、`corepack pnpm run build` 均通过。
- 契约/仓库静态门禁：IPC command contract（79 handlers）、IPC events（36 channels）、ADR index（1133 entries）和 crate dependency inventory 均通过。

当前数据库 v42 重置要求及旧版拒绝行为见 [release-and-reset.md](release-and-reset.md)。本记录不关闭 ADR 0395 的 Windows 发布安装包验收 Open Gate。
