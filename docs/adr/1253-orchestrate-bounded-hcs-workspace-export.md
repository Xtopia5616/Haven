# ADR 1253：编排有界 HCS workspace 输出导出

## 状态

已增加平台内部 export/decode orchestration；尚未由 broker service/provider 调用，也未启动真实 HCS system 或执行 quota 验收。WorkspaceWrite quota gate 继续 fail closed。

## 背景

ADR 1248 的 workspace-output decoder、ADR 1249 的 stopped-layer plan、ADR 1250 的隔离同步 helper，以及 ADR 1251/1252 的 bounded transport 与身份核验此前各自存在，但没有单一 API 负责它们的 owner 顺序。调用方需要保证 export 只发生在 stopped proof 成立后，transport 与 output 同属一个 attempt，并在 native 行为确定静止后才回收磁盘。

## 决定

- 新增 export_stopped_workspace_output：在 attempt 内创建固定 workspace-output staging，创建有界 transport owner，使用 stopped-system proof 与 closed config 构造 worker plan，再调用隔离 HCS export helper，最后只提取 reserved workspace subtree。
- worker timeout 与 transport VHDX 容量在任何 staging 或特权存储操作前校验；不合法的请求不会创建或挂载 transport。
- helper 完成且 output 解码后，显式 detach 并清理 transport VHDX，随后才将 attempt marker 从 active 清空。成功时把 output capability 返回给调用方，attempt 继续由调用方持有以完成 output diff/review/transfer。
- plan 构造、worker 或 decoder 的确定失败只有在 transport 已显式清理且 marker 可安全清空后才返回 cleanup_safe=true；transport 建立、detach、marker 清除失败会保留 owner marker。worker timeout/cancel 或不能确认进程树已结束时立即保留 transport 与 attempt quarantine，不尝试 detach、解码或删除。
- 输出 staging 与 workspace-input 同处一个私有 attempt 的兄弟目录；重试若发现已存在 staging 则拒绝覆盖，调用方必须清理整个失败 attempt。
- 此 orchestration 只组合已实现的 typed owners，不创建 compute system、不选择镜像层、不调用 HcsCreateProcess，也不改变服务 quota gate 或工具授权。生产 service/provider 接线仍需单独实现；真实 Windows HCS 格式、挂载、导出及强制终止行为未验收。

## 替代方案

- 由多个调用方各自拼接 owner/helper/decoder：拒绝；容易遗漏 quota transport、marker 或 teardown 顺序。
- helper timeout 后立即 detach：拒绝；HCS API 未定义 caller 被终止后的 host-side 写入，继续按 ADR 1250/1251 quarantine。
- 把部分 decoder output 晋升或传输：拒绝；decoder 失败时整个 attempt 仍是私有临时数据。

## 影响与迁移

新增 broker-private execution API 和 workspace-output attempt 子目录。所有目录仍由 BrokerWorkspaceAttempt 统一清理；不改变 broker wire、用户配置、数据库或公开 Tools API，无需重置。此函数未接入 live service，现有请求仍在 quota gate 之前失败。

## 验证

- cargo fmt --package haven-platform -- --check、cargo test --locked -p haven-platform、cargo clippy --locked -p haven-platform -- -D warnings 与 ADR index checker。
- 单元测试验证 output staging 与 input 的 attempt 内兄弟关系、拒绝覆盖和失败分类。没有调用 HCS formatter、compute-system、guest process、layer export 或超额写入验收。

## 回滚

可移除该 orchestration API 和固定 output 子目录创建逻辑；provider 接线前回滚不影响当前 broker request。quota gate 必须继续 fail closed，attempt 不确定状态不得自动清除。
