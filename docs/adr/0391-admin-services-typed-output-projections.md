# ADR 0391：AdminServices 固定输出的 typed projections

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0070](0070-restricted-admin-surface.md)、[ADR 0071](0071-typed-tool-operation.md)、[ADR 0383](0383-stage3-typed-memory-and-action-ports.md)
- 清单：[跨层输出契约清单](../architecture-output-contract-inventory.md)

## 背景

ADR 0383 明确没有完成全仓跨层输出审计。清单逐项检查后，将 `AdminServices` 的 diagnostics、logs、session、skills、MCP 状态和 mutation acknowledgements 分类为稳定形状；只有配置任意路径读取、脱敏设置树等动态配置内容需要继续使用 JSON。

先前 `AdminServices` 将这些固定结果构造成 `serde_json::Value`，调用方难以从 Rust 签名看出字段、可选分支和省略语义。模型可见工具的最终输出仍是 JSON，但不要求服务层提前抹除类型。

## 决定

1. `AdminServices` 固定输出使用具名 DTO 或闭合 enum：diagnostics/status、logs tail、sessions/errors、skills list、log/tool/skill/MCP acknowledgements、MCP status/reload 和 renderer native MCP reconnect/refresh。
2. `AdminOperationOutput` 使用 untagged union，将服务类型带到 `TypedToolAdapter` 与 `AdminSurfaces.execute` 的现有工具输出序列化边界。它只序列化各分支的内容，不新增 discriminator、字段或包装层。Config operation 继续由 `ConfigOperationOutput` 在相同边界映射。
3. 字段和值保持原 wire 契约：MCP status 的 `diagnostic` 即使为空也输出 `null`；`mcp_add` 新增分支的 `warning` 只在连接失败后出现，重复名称走配置更新 ack 且不输出 warning；MCP reload 的成功行省略 `error`，失败行包含经 sanitizer 过滤的 `error`。列表顺序和空数组沿用现有生产者顺序与行为。
4. Session rows 只输出 id/status/title/input 字符数/时间，不输出 transcript。MCP status 和 mutation ack 不输出 command、args、env 或凭据；日志行继续按敏感 marker 整行脱敏并限制长度；诊断字段、MCP 连接错误与 AdminOperationError 继续经过现有 sanitizer。logs-tail 文件读取错误保留原有 I/O 文案形状。
5. `config_get` 任意路径保留 `Value`，完整 Settings 的敏感值继续由 `AdminServices` mask 后返回。`diagnostics_status.settings` 是同一类动态配置树；provider、模型工具/MCP schema、工具参数与输出等动态协议载荷仍由其既有 owner 持有，不在此 ADR 中 DTO 化。

`admin_services.rs` 仍同时容纳 Admin 域服务与其返回投影，文件超过约 800 行。当前 DTO 只服务这些紧邻的 producer 和单一 Admin wire 边界，拆分会增加文件跳转而不减少职责；保留理由是类型与字段组装保持共址。若其它模块开始复用这些 DTO 或 Admin 增加独立领域职责，再拆为内部 output 子模块。`admin.rs` 继续作为 Admin operation 名称/schema、native request bridge、adapter 与 output boundary 的单一权威；测试仍与私有 operation 实现同模块，以便通过生产入口验收。未来出现独立子域时再拆 operation modules 与测试夹具。

## Owner 与序列化边界

| 输出 | 生产者与消费者 | Owner / 序列化 |
|---|---|---|
| diagnostics、logs tail、sessions/errors | `AdminServices` 聚合 `ConfigService`、router、registry、`SessionStore`、日志文件；Diagnostics operation 消费 | Admin 拥有聚合、mask 和 sanitizer；`TypedToolAdapter`/`AdminSurfaces.execute` 负责最终工具 JSON |
| skills list / mutations | `SkillsEngine` 提供元数据，Admin operation 调用服务 | Skills 拥有扫描元数据；Admin 拥有 mutation 与 ack；工具输出边界序列化 |
| log/tool settings acknowledgements | `ConfigService`、`ToolControlPort` 经 `AdminServices` | Admin 服务返回具名结果；Config operation 或工具输出边界序列化 |
| MCP status、mutations、reload、native reconnect/refresh | `McpManager` 与配置由 `AdminServices` 聚合；Admin operation 或 App native caller 消费 `ToolResult.output` | Admin 拥有安全裁剪、错误过滤和结果投影；既有 tool-output 边界序列化。App 可继续从 native refresh JSON 读取授权计划对应的 `failed` 名称 |
| 配置树与任意配置路径 | `ConfigService` 配置快照由 Config operation 消费 | `ConfigService` 拥有配置；Admin 拥有敏感字段 mask；Config tool output 边界最终序列化 |

## 影响与验证

本 ADR 只改变服务层 Rust 返回类型与类型化适配，不改变模型可见工具名称、输入 schema、成功/失败字段、配置格式、MCP 协议或 Tauri IPC。回归测试比较工具边界的完整 JSON shape，覆盖可选字段、`null`、空与 unavailable、列表顺序、部分失败、敏感值过滤和 transcript 排除；夹具使用临时目录与内存数据库，不访问用户目录或远端网络。

```text
cargo fmt --check
cargo test --locked -p haven-tools
cargo clippy --locked -p haven-tools -- -D warnings
```

若该 crate 类型签名影响 App 编译，再运行 `cargo check --locked -p haven-app-binary`。

## 回滚

回退 `AdminServices` 的 DTO 与 operation output union，恢复原 JSON 构造即可；不需要数据库、配置或缓存重置。
