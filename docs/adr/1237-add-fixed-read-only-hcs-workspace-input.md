# ADR 1237：为 HCS 容器增加固定只读 workspace 输入映射

## 状态

已实施 HCS 配置序列化与平台单元测试；broker provider 尚未创建 compute system 或接入此字段。运行期配额验收仍由 ADR 1231 阻断生产执行。

## 背景

ADR 1221 为 HCS 配置定义了闭合字段集合，避免调用方传入任意宿主挂载。后续 typed 执行端口已将 workspace 输入限制为 capability，并规定 provider 只能将有界文件树复制到 attempt 私有 staging（ADR 1224）。容器启动时仍需要把该快照提供给 guest；若配置继续完全禁止挂载，broker 无法交付输入文件。

## 决定

- HCS 配置只增加一个可选 `MappedDirectories` 项，来源必须是 broker 持有的 `DirectoryCapability`，表示单次 attempt 的私有 workspace 输入 staging。
- guest 路径固定为 `C:\\Haven\\workspace-input`，`ReadOnly` 固定为 `true`；带宽和 IOPS 上限字段固定为零，`CreateInUtilityVM` 固定为 `false`。调用方不能指定 guest 路径、读写标志或额外挂载。
- 配置对象持有 capability，以便 HCS 使用该路径期间保留已打开的目录身份。HCS 映射本身仍依赖 broker 在创建映射前完成受限复制和 staging 生命周期管理。
- 没有 workspace 输入时省略 `MappedDirectories`，保留原来的 HCS JSON 字段集合。此序列化能力不证明容器已经启动、配额已强制或 workspace 写入权限可开放。
- Shell/Tools 尚未连接 HCS provider；quota runtime attestation 和 guest 超额写入验收继续作为生产执行硬门槛，未验收时 broker 仍 fail closed。

## 替代方案

- 继续完全禁止映射：guest 无法读取本次 attempt 的输入文件，不能实现 workspace 执行。
- 允许任意 host/guest 路径、可写映射或任意数量映射：拒绝，会扩大高权限 broker 可触达的宿主文件面，并绕开 workspace 快照与输出审阅边界。
- 把 workspace 内容内嵌到 HCS JSON 或 broker 控制帧：拒绝，内容可能远大于控制协议预算，也会混淆控制面与有界文件树数据面。

## 影响与迁移

仅新增平台内部配置字段及测试，不改变 Tauri/MCP/数据库/用户配置契约，无需重置数据或配置。ADR 1221 的“无 host mount”决策由本 ADR 限定扩展：仍禁止任意或可写映射，只允许这里定义的单一只读输入映射。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- 测试锁定无映射时的旧字段集合，以及映射数量、固定 guest 路径、只读属性和所有序列化字段；重复附加 capability 必须失败。
- 本切片不运行 HCS compute system。Windows 容器创建、运行期配额 attestation、guest 超额写入和真实文件交付仍需专用验收环境。

## 回滚

删除 HCS 配置中的 workspace capability、`MappedDirectories` 类型和对应测试即可回滚；没有生产 provider 消费该字段，不影响现有工具行为。
