# ADR 1246：将经验证的 attempt scratch 绑定到 HCS writable layer folder

## 状态

已实现固定 HCS 配置契约与负向单测；尚未在 HCS 目标机创建/启动 compute system，也未验证 guest 配额。broker quota gate 继续 fail closed。

## 背景

ADR 1227–1230 建立了 attempt 私有 `sandbox.vhdx`、容量元数据复核与格式化原语，同时明确这些检查不能证明 HCS 消费该磁盘或 guest 写入受硬配额约束。HCS compute-system owner 已有固定配置构造器，但该配置尚未引用 attempt writable layer，因而无法把 scratch VHD 的容量与 HCS 容器文档关联起来。

Microsoft HCS v1 container 配置使用 `LayerFolderPath` 指定 writable layer 文件夹；Microsoft 的 hcsshim WCOW 配置构造也为 HCS v1 维护该字段。此字段接线仍只是配置契约，不是运行期配额证据。

## 决定

- `WindowsContainerSandboxConfig` 序列化前必须绑定一个本 attempt writable-layer 文件夹，并将其写入固定 `LayerFolderPath` 字段。未绑定时配置拒绝序列化。
- 生产构造只能接收 broker 内部 `BoundedScratchVhd`。该适配在调用 `layer_path()` 时要求 HCS formatter 已成功，并要求 VHDX 的已验证 `MaximumSize` 与请求中的存储上限完全一致；普通调用方不能传任意 host path。
- 保持现有 `StorageSandboxSize` 字段并与 scratch 容量关联，但不把它、`LayerFolderPath` 或 VHDX 元数据本身视为 HCS 运行期容量证明。
- HCS JSON 本地测试使用测试专用路径注入。生产 API 没有此注入入口。
- `HcsComputeSystemOwner` 尚未接入 broker 请求；quota runtime attestation 仍在 staging 分配和 workspace 字节读取之前拒绝所有执行。只有取得目标 host 实测结果后，才可接通 service/provider。

## 替代方案

- 继续只序列化 `StorageSandboxSize`：拒绝；这不会让 HCS 配置引用 broker 创建的 attempt scratch。
- 让 IPC 请求提供 `LayerFolderPath`：拒绝；客户端不得传任意 host path 或 HCS 配置。
- 仅凭 `LayerFolderPath` 与 VHDX `MaximumSize` 开放执行：拒绝；这两项不证明 HCS 以预期模式挂载该盘或限制 guest 写入。

## 影响与迁移

这是未接入执行路径的内部平台配置变化，无数据库、用户配置、IPC、授权或持久化数据迁移。所有调用 HCS config serializer 的生产路径现在都必须显式提供已格式化的 attempt layer。测试版本没有向下兼容要求。

## 验证

- Microsoft [HCS v1 container 配置构造](https://github.com/microsoft/hcsshim/blob/main/internal/hcsoci/hcsdoc_wcow.go)使用 `LayerFolderPath` 表达容器 writable layer folder。
- crate 门禁通过：`cargo fmt --package haven-platform -- --check`、`cargo check --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings`；`cargo test --locked -p haven-platform` 为 111 passed、1 ignored。
- 没有运行 HCS formatter 特权探针、compute-system 启动或 guest 超额写入测试；不得将单测或本机源码推理当成配额验收。

## 回滚

移除 `LayerFolderPath` 绑定入口可恢复到未接线的配置原型；broker quota gate 必须保持 fail closed。若目标机验证未能证明预期 HCS 消费协议，应关闭可选 HCS backend，不能退回共享可写 host mount 或仅依靠 `StorageSandboxSize`。
