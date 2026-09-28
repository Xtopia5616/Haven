# ADR 0394：阶段 8 IPC 类型生成与聊天编排收口

## 状态

已采纳并实现（2026-09-29）。

## 背景

阶段 8 已逐域建立 Tauri command DTO、前端类型、mapper、调用 helper 和文档，但请求/响应字段仍在 Rust handler/Serde DTO、`contracts/commands.ts`、Rust command registry 与 IPC 脚本中重复描述。新增字段可能造成编译仍通过、手写 contract 却漂移；公共 `invoke` 也无法在调用点校验 command 名和参数类型。

聊天路由还保留 ask/input 最终分流、启动恢复编排、滚动跟随与 DOM observer 生命周期。这些状态和副作用已经有明确 owner，但留在一个页面文件里。

## 决定

1. Rust Tauri handler 参数和 Rust DTO/Serde 属性是命令 request/response wire shape 的唯一来源。`generate_ipc_contracts` 从 workspace Rust 源解析这些签名并生成 `ui/src/lib/contracts/generatedCommands.ts`；request DTO 必须有 `Deserialize`，response DTO 必须有 `Serialize`，并分别生成 `FooInput` 与 `Foo`。输入字段遵循 `Option`、字段/容器 `serde(default)` 和 `skip_deserializing`；输出字段遵循 `skip_serializing`、`skip_serializing_if` 与 `skip`，因此默认值不会错误地削弱 response。生成器显式读取 `settings_pair!` 的实际字段声明和默认属性。无法确定的类型或影响 wire shape 的未知 Serde 属性会使生成失败。只有显式动态扩展点（例如 `serde_json::Value`）映射为 TypeScript `unknown`。
2. `scripts/check-ipc-contracts.ps1` 执行生成物 drift 检查，并核对 live handlers、Tauri 注册、Rust registry、前端 boundary/security registry 和文档的命令名集合。Rust/前端 registry 只保留 command name、boundary 和人工审阅的 security 描述，不重复 request/response 字段或 DTO 名称。
3. 前端公开 `invoke` 受生成的 command name、request 和 response 类型约束；底层 Tauri bridge 仍以 `unknown` 处理数据。已有 settings、action、MCP、diagnostics 等结构化响应继续经过运行时 parser/validator；生成的静态类型不被视为运行时信任边界。
4. 聊天路由把 ask/input 分流、会话启动恢复和滚动/observer 状态分别委托给 `chatAskInteraction`、`chatSessionStartup`、`chatViewController`。页面保留组件接线及其他页面 UI 状态；既有 `ChatController`、事件 mapper、session reducer 和 store 继续拥有原职责。

## 考虑过的方案

- 继续手动维护每个 command 的 TypeScript request/response 字段并逐项比较：字段清单仍有多个人工副本，新增字段需要同步多处，因此不采纳。
- 在 Rust crate 的正常构建过程中运行 codegen：这会把生成器解析依赖和生成副作用带入应用构建。改为通过 `ipc-codegen` 可选 feature 和显式脚本运行，普通应用构建不启用解析器。
- 删除运行时 payload parser，只依赖 TypeScript 类型：Tauri IPC 数据在运行时仍是外部 wire input，静态类型不能校验恶意或畸形响应，因此不采纳。
- 将所有聊天 UI 状态搬入一个总 controller：会把启动、ask 和 DOM 生命周期重新集中到另一个大对象，因此按稳定职责拆成三个纯 TypeScript owner。

## 影响

- Rust 命令 wire 字段、Serde 序列化语义、命令集合、事件 channel、数据库 schema、ID、持久化顺序和用户可见交互保持不变。
- 新增或修改 command/DTO 后需运行 `scripts/generate-ipc-contracts.ps1` 更新生成文件；CI drift 检查要求提交生成结果。
- 解析器对未知 wire 语义 fail closed。新增 Serde serializer、字段变体或 Rust 类型时，需为生成器增加明确映射及测试后才能进入 IPC contract。
- 本 ADR 不改变 application data；无需数据库或配置重置。

## 验证与回滚

- 验证包括 generator 单元测试、IPC command/event 检查、Rust format/check/Clippy/workspace tests，以及 UI type check/test/production build。
- 若需回滚，删除可选 codegen binary、生成文件及其检查入口，恢复手写 request/response contract 和旧的 UI owner；wire handlers 与 Serde DTO 不需迁移或重置。
