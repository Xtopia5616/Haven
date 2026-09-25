# ADR 0331：ToolsManager typed capability snapshot 与缓存失效边界

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` 的媒体、录音/STT 与 web-search runtime capability 读取
- 关联：[ADR 0211](0211-operation-registry-and-platform-snapshot.md)、[ADR 0326](0326-tools-runtime-capability-resolution.md)

## 背景与不变量

ADR 0326 将 provider、MCP 与媒体 capability policy 从 `ToolsManager` 抽到
`runtime_capabilities`，但 manager 的 prompt、TTS/STT gate、recording transcription
以及 builtin catalog 构建仍分别解析能力。能力输入还来自不同更新路径：
`PlatformRuntime` 整份替换；config coordinator 发布 Router runtime；MCP manager
独立更新 client 和 `tools/list` cache，并维护自己的 `catalog_version`。这些来源没有
共同版本钟或原子发布 owner。

本切片保持以下不变量：

1. Web search 仍按 provider、MCP、unavailable 选择；provider 必须有已配置的 Chat route、
   非关闭的搜索模式及支持 built-in search 的 adapter。MCP 识别只读已构建 index 中的工具名。
2. Dedicated STT 和 transcription route 继续提供转写；录音只由 capture pipeline 决定，
   无 STT 时仍允许录音。vision、OCR、image generation 与 TTS 来源不变。
3. 媒体 operation schema 与 prompt/runtime capability 由同一次 typed snapshot 派生；
   transcribe ingress 与录音转写 gate 也读取同一 snapshot 类型。
4. 工具授权和执行前 live runtime 校验仍归既有执行边界；snapshot 不是授权决策，也不替代执行复验。
5. 不改变 `PlatformRuntime` 生命周期、MCP 连接管理、配置应用顺序、错误/通知语义、IPC 或动态工具参数。

## 决定

1. `runtime_capabilities::ToolCapabilitySnapshot` 是 crate-private typed value，只含媒体能力和
   `WebSearchAvailability`，不持有 Router、MCP manager、Database 或 service facade。
2. `ToolsManager` 唯一负责构造该 snapshot。每次 public capability read 都从当前
   `PlatformRuntime`、当前 Router config 与新构建的 MCP index 重新解析；TTS、transcription
   availability、recording transcription 和 prompt-facing `RuntimeCapabilities` 都从其结果读取。
3. 每次 builtin catalog rebuild 只读取一次 `PlatformRuntime`，由相同 platform generation
   构造 snapshot，并将其中的媒体能力传入 `MediaDeps`。媒体 operation schema 不再自行重新解析
   Router/STT/录音能力。
4. 当前不缓存 snapshot。即使 MCP 有 `catalog_version`，它也不覆盖 Router config 发布与
   `PlatformRuntime` 的更新；建立组合版本或协调发布 owner 前，缓存可能返回旧能力。现有目录版本
   继续用于工具定义目录，能力读取直接重新验证来源。

## 替代方案

- 在 `ToolsManager` 或 `ToolRuntime` 中缓存 snapshot 并分别跟踪 platform、Router 与 MCP 版本：
  会建立一个不完整的失效时钟，而且当前没有单一原子更新路径，拒绝。
- 只保留独立媒体 resolver，让 prompt、catalog 和 ingress 分别计算：会继续有多处能力读取点，
  配置变化时难以保证 schema 与 prompt 对齐，拒绝。
- 将 Router/MCP 生命周期迁入 ToolsManager：扩大跨 crate composition 和连接生命周期边界，超出本切片，拒绝。

## 影响与验证

- 无 schema、配置、provider contract、授权、工具动态参数、IPC 或数据重置变化。
- 测试覆盖 typed snapshot 的 media 来源与 prompt 投影、provider/MCP 优先级、runtime 替换后
  fresh snapshot 与 TTS/STT/录音读取路径、媒体 catalog operation pruning，以及并发读取与 runtime
  替换。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、
  `cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、
  `cargo test --workspace --locked`、`git diff --cached --check`。

## 回滚

回滚时移除 `ToolCapabilitySnapshot` 和 manager 构造入口，恢复 `ToolBuiltins` 与 ingress/TTS/STT
各自读取 capability resolver，并撤回本 ADR、ADR 索引、架构说明和路线图更新。无持久化或 wire 数据
需要迁移。
