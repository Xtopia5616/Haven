# ADR 1221：构造封闭的 HCS 容器配置

## 状态

已实施配置序列化原语；未创建 HCS compute system、未安装 broker，也未连接工具执行。运行期配额验收仍由 ADR 1220 阻断。

## 背景

ADR 1220 选择显式可选的 brokered Windows Container sandbox。首个实现切片需要把每次执行配置固定为可审查的 HCS V1 文档，同时避免调用方传入任意 HCS JSON、host mount 或网络端点。单元测试可以证明序列化契约，但不能证明目标 Windows 主机实际执行该存储配额。

## 决定

- `haven-platform::windows_container` 只提供固定 HCS V1 container 配置构造器。隔离类型固定为 `Container` + `HvPartition=true`；配置包含 broker 名称、容器名称、受信任镜像 layer 列表、非零 `StorageSandboxSize` 字节值和 `TerminateOnLastHandleClosed=true`。
- 配置使用闭合 Rust 结构序列化，不接受任意 JSON 扩展。当前可序列化字段集合通过测试精确锁定；没有 host 可写映射、网络 endpoint、凭据、设备或 caller-selected runtime 字段。
- Layer 构造只接受本地绝对 Unicode 路径，拒绝 UNC、相对路径、`.`/`..` 与控制字符。此语法检查不证明镜像来源、文件身份或 reparse 状态；未来 broker 必须先从受信任镜像目录查找固定 image profile，并验证其目录 capability/身份，再创建 layer descriptor。IPC 不得暴露 layer path 或 ID。
- 此构造器只写入调用方提供的非零配额值；它不拥有每次执行上限、并发预算或磁盘空间 owner。配额策略和 HCS runtime 查询/超额写入证明属于后续实现切片。没有实际 Windows 运行期证据时，不得据此启用 `WorkspaceWrite` 下的 opaque 子进程。
- 不创建容器、不检查或改变系统功能、不连接 broker、不修改权限、配置或数据库。Tools 当前 fail-closed 行为不变。

## 替代方案

- 由调用方提交任意 HCS JSON：拒绝；会让高权限 broker 接受未审查的字段和宿主资源映射。
- 让工具 handler 直接构造 HCS 配置：拒绝；镜像选择与系统 API 属于平台执行边界，授权和确认仍由 Tools 所有。
- 在本切片即启动容器并视正向序列化测试为配额证明：拒绝；HCS schema 接受字段并不证明目标 host build 运行期 enforcement 有效。

## 影响与迁移

该切片新增平台内部依赖 `serde`/`serde_json`、配置构造模块和单元测试；不改变 IPC、持久化配置、数据库 schema 或工具权限，不需要数据/配置重置。后续 broker 接口仍应传递 typed profile 与执行请求，不传 HCS JSON、镜像路径或 host mount。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- Windows 单元测试精确断言隔离模式、字节配额、字段白名单、trusted layer JSON，以及非法 layer 路径、零配额和空 layer 列表拒绝。
- 本机尚未运行 HCS compute system 或超额写入探针；Windows 目标版本支持矩阵、guest 容量错误、host 卷可用性和 scratch 清理由 ADR 1220 保持为接线前硬门槛。

## 回滚

删除 `windows_container` 配置构造器及其直接序列化依赖即可回滚。本切片未接入执行路径，回滚不改变运行时权限或工具行为。
