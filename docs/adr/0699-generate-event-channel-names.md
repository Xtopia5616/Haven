# ADR 0699：从 Rust 事件目录生成 UI 通道名

## 状态

已采纳并实施。

## 背景

`crates/app-binary/src/events.rs` 已集中定义 35 个 Tauri event channel 常量。UI 的 App、Agent、recording、ToolRun contracts 又分别手写同一组 channel 名；Session lifecycle mapper 和 listener 还单独写了 `session:lifecycle`。`check-ipc-events.ps1` 只能在两份列表之间做集合比较，新增通道仍需人工同步多处。

## 决定

- IPC generator 读取 Rust event channel 常量，并按常量 owner 分组成 `APP_EVENT_NAMES`、`AGENT_EVENT_NAMES`、`RECORDING_EVENT_NAMES`、`SESSION_EVENT_NAMES` 和 `TOOL_RUN_EVENT_NAMES`，生成到 `generatedCommands.ts`。
- UI contract 与 Session listener 直接消费生成数组；删除手写事件目录和只验证同一组字面量的 recording 测试。
- IPC event 检查比较 Rust 目录与生成数组，并确认各 UI contract 消费对应数组。
- Rust payload DTO 与事件 producer 保持原 owner；UI payload map、字段校验和 snake_case→camelCase 投影仍由各自 contract 负责。

## 替代方案

- 保留 Rust 与 UI 两份列表，仅用漂移脚本比较：拒绝。名称源仍有两份，编辑时仍要求同步维护。
- 将 channel 常量移出 Rust events module：拒绝。当前生产者已统一引用该事件目录；代码生成可以直接消费它，不需新建另一个 Rust owner。
- 把事件注册与各域 payload mapper 合成一个通用事件框架：拒绝。通道名来源重复不代表 payload 语义与映射职责相同。

## 影响与验证

- 35 个 event channel 字符串、payload shape、事件顺序和运行时行为不变；没有持久化数据、配置或用户数据影响，无需重置。
- 验证：IPC generator 单元测试、事件通道一致性检查、生成契约漂移检查、Rust workspace 测试与 Clippy、UI check/test/build。

## 回滚

恢复手写 UI channel 数组及其同步检查即可；无数据迁移。
