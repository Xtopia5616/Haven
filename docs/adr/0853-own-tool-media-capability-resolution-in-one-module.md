# ADR 0853：由单一模块拥有工具媒体能力解析

## 状态

Accepted — 2026-10-10

## 背景

`haven-tools` 有两个同名的 `resolve_media_capabilities`：`builtin` 中的函数判断 Router 路由及专用 STT
客户端能否支持媒体操作；`runtime_capabilities` 中的函数再把该结果与录音、OCR、图像生成和 TTS
运行时客户端合并，供 `ToolCapabilitySnapshot`、prompt、媒体工具目录和录音转写入口使用。虽然调用处有
模块限定名，两段能力策略仍分散在 builtin catalog 与最终 capability snapshot 两个模块；裸函数名也没有
表达各自覆盖的范围。

## 决定

- 将 Router 路由和专用 STT 判断迁入 `runtime_capabilities`，与本地媒体服务的合成保持同一模块 owner。
- 将内部辅助函数分别命名为 `resolve_backend_media_capabilities` 和
  `resolve_tool_media_capabilities`：前者只负责 Router/专用 STT，后者产生最终供工具使用的能力值。
- 从 `builtin` 删除旧 resolver；两个新函数保持模块私有，唯一对外入口仍是 `resolve_snapshot`。
- 保留能力位含义、路由探测顺序、回退规则和最终 `ToolCapabilitySnapshot` 形状，不新增缓存或版本钟。

## 替代方案

- 保留两个同名函数，依赖模块限定名区分：拒绝。调用路径可编译但读者仍需跳转实现才能判断哪一个包含运行时客户端。
- 将全部解析强行压成一个大函数：拒绝。Router/STT 后端判断与本地服务合成有不同输入，两个阶段仍需具名表达；它们由一个模块统一拥有即可。

## 影响与验证

这是 `haven-tools` 内部函数位置与名称调整；没有跨 crate API、IPC/event、持久化、配置或用户行为变化，无需数据重置。
验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、
`cargo clippy --locked -p haven-tools -- -D warnings`、ADR index 检查与 `git diff --check`。
未运行测试套件；现有 runtime capability 投影测试保持不变，本次通过完整保留 resolver 判定逻辑控制行为面。

## 回滚

若行为验证发现差异，整体恢复 `builtin::resolve_media_capabilities` 和其调用方；不得重新添加两个同名的
resolver 作为兼容入口。
