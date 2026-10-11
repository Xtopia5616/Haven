# 0932：让媒体 contracts 只经 Common media 子域导出

## 状态

已接受并实现（2026-10-11）。

## 背景

Common 媒体 contract 分属 `media`（资产、表示、计划和 capability）与 `media_detection`（探测类型和 MIME/签名函数）两个领域模块。根模块重复 re-export 了两组 API；全仓消费者几乎全部使用领域模块，余下根路径只存在于四个 provider adapter 对 capability profile 的导入，以及 attachment ingress 的媒体探测调用。

## 决定

- `haven_common::media` 是资产、表示、计划和 capability contract 的唯一入口。
- `haven_common::media_detection` 是探测结果和探测函数的唯一入口。
- 删除 Common root 上的两组媒体 re-export；消费者迁移到各自模块路径。
- 不合并两个模块：媒体规划/资产模型与内容探测函数有不同职责，独立命名有助于避免误以为所有操作都属于同一生命周期 owner。

## 影响

这是未发布 workspace API 的破坏性收口，不保留根 aliases。类型、序列化形状、探测规则与运行行为不变。

## 验证

- 静态搜索确认生产与测试消费者使用 `media` / `media_detection` 路径，且无 Common root media alias 导入。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 同时保留根与命名子域：拒绝，形成无需的双入口。
- 把探测函数并入 `media`：拒绝，探测输入内容签名与 media planning 使用 provider capability 生成投影/计划，前者不需要构造或拥有后者的状态。

## 回滚

若未来 Common consumer 证明根路径应为唯一入口，应收回相应命名模块并迁移所有消费者；不得恢复双路径。
