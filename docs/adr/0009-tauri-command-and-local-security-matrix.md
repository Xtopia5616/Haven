# ADR 0009：全量 Tauri 命令目录与本机工具安全矩阵

## 背景

Haven 的 Tauri 命令分布在多个 app command module，过去只有会话、任务和少数
设置/录音边界被显式登记。其余命令的请求字段、响应类型、敏感字段和安全入口
需要依赖人工搜索，容易出现 handler、`generate_handler!`、前端调用和文档漂移。
本机工具还需要验证重解析点、授权继承、定时任务二次检查和取消语义，不能只测
“需要弹窗”这一条正向路径。

## 决定

1. 使用 `crates/app-binary/src/commands/contracts.rs` 作为 v1 命令目录唯一 Rust
   登记点，当前覆盖 67 个 `#[tauri::command]`。每项包含命令名、请求 DTO 名称、
   响应 DTO/标量、边界类型和安全不变量。
2. 保持当前 Tauri 的扁平请求 wire shape；DTO 名称是稳定 schema 标识，不把请求
   额外包成 `{ request: ... }`。现有 camelCase 前端调用不需要整体迁移。
3. 使用 `ui/src/lib/contracts/commands.ts` 镜像完整命令目录，并由
   `scripts/check-ipc-contracts.ps1` 比较 handler、`generate_handler!`、Rust 目录、
   前端目录和 `docs/ipc-contracts.md`。任何缺失、重复或多余命令都阻断 CI。
   事件则由 `crates/app-binary/src/events.rs` 与前端各域 contract 组成完整目录，
   `scripts/check-ipc-events.ps1` 校验两侧的 39 个 channel 集合。
4. 对稳定业务响应移除无必要的 `serde_json::Value` 外壳：MCP/skill 执行结果、
   工具列表、记忆召回、模型发现和转写结果使用命名 DTO；provider 原始载荷、
   动态工具 schema 和工具输出仍可使用 `Value`。
5. `SafetyGateway` 的授权继承采用“整条 ancestry 先扫 deny，再扫 allow”：子级
   allow 不能绕过父级 deny，子级 deny 不能被父级 allow 绕过；permanent deny
   压过 session allow。所有 adapter 使用稳定的 qualified name，session grant
   不泄漏到无 session 入口。
6. allowed path 校验先拒绝相对/UNC/device 路径，再 canonicalize 现有前缀并拒绝
   任一 symlink 或 Windows reparse point，最后才做边界比较；源、目标、cwd、数组
   路径逐项检查。执行入口仍需在实际执行前重复 gate 以降低 TOCTOU 风险。
7. `docs/security-regression-matrix.md` 是本机工具的人工审查矩阵，
   `LOCAL_TOOL_SECURITY_MATRIX` 和 SafetyGateway 测试是可执行的核心代表集。
   测试不触碰真实用户目录、注册表、电源、网络或桌面输入。

## 替代方案

- 仅维护 Markdown：可读但无法发现 handler/前端/注册表漂移，拒绝。
- 统一把所有命令改成 `{ request: ... }`：类型边界更直观，但会破坏当前 renderer
  wire shape，且不能替代安全矩阵，暂不采用。
- 只依赖 lexical path prefix：实现简单，但 symlink/junction/reparse point 可
  把路径解析到 allowed root 外，拒绝。

## 影响

- 新增命令必须同时更新 Rust registry、前端 registry、契约文档和脚本校验。
- `mcp_tool_call`、`execute_skill`、`process_transcript` 等返回值的外层 JSON 形状
  保持兼容；变更仅把实现从匿名 JSON 改为命名 DTO。
- 不存在的路径允许在已 canonicalize 的可信父目录下继续比较；现有路径只要遇到
  reparse point 即拒绝，可能比旧 lexical 行为更严格。
- 旧的 68 命令描述按实际 handler 数量修正为 67；没有数据库迁移。若未来需要
  改 wire shape，应提升 IPC contract version 并在 release/reset 文档说明。

## 验证

```text
./scripts/check-ipc-contracts.ps1
./scripts/check-ipc-events.ps1
cargo fmt --check
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace -- --test-threads=1
cd ui && npm run check && npm run test:run && npm run build
```

重点回归测试包括：parent/child allow-deny precedence、permanent/session precedence、
MCP/skill adapter key sharing and isolation、relative/UNC/source+destination path、
Unix symlink escape、disabled operation、threshold reset、以及矩阵中每个 builtin
tool family 的风险门禁。

## 回滚与重置

这是代码和文档契约变更，不新增持久化 schema。回滚时必须同时移除命令目录脚本、
前端目录和矩阵，不能只删除某一侧登记。若未来 contract version 或 wire shape
不兼容，删除旧构建生成的 `haven.db`、配置和缓存后重新初始化，并在发布说明中
明确重置范围。
