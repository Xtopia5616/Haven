# ADR 0581：明确 capture engine 角色与重采样动作

## 状态

已采纳并实施。

## 背景

Input 录音管线里的 `Engine`、`EngineCommand`、`EngineHandle` 脱离 `capture` 模块后不能从类型名辨认 owner。`EngineHandle::drain_shared` 描述 mutex 共享实现，而消费方实际需要知道它取出活动 capture buffer 当前已有的 PCM。`Resampler::process` / `process_into` 则没有表示状态式采样率转换动作。

## 决定

1. engine owner、命令和客户端句柄分别改名为 `CaptureEngine`、`CaptureEngineCommand`、`CaptureEngineHandle`。
2. 活动 capture buffer 的直接读取改为 `drain_buffered`。
3. 重采样转换入口改为 `resample` 和 `resample_into`。
4. 保留 `capture::spawn_engine`（模块路径已说明领域）；保留 RingBuffer 的常规 `push`、`drain`、`clear` 以及 backend 的 `start` / `stop`。

## 替代方案

- 保留 `drain_shared`：拒绝，名称暴露锁/共享实现，未说明取得的业务数据。
- 将 `spawn_engine` 改为 `spawn_capture_engine`：拒绝，调用路径已经是 `capture::spawn_engine`，额外重复模块语义。
- 将 Resampler 转换方法统一留作 `process`：拒绝，类型名与函数名合并阅读仍不能看出采样率变换。

## 影响与验证

- 同步 Input pipeline、capture backend、resampler 测试和内部 engine 命令分发调用点。
- 线程生命周期、ring 所有权、PCM 读取、重采样算法和输出均不变。
- 验证：Rust `fmt --check`、workspace `check`、严格 Clippy、workspace serial tests、ADR 索引与差异空白检查。

## 回滚

将 Capture engine 类型名、`drain_buffered` 和 resampler 方法恢复为旧名称，并同步还原所有 Input 调用点与命名文档。
