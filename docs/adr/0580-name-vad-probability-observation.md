# ADR 0580：明确 VAD 概率推理与观察入口

## 状态

已采纳并实施。

## 背景

`VadEngine::infer(frame)` 和 `VadWorker::infer(frame)` 接收音频帧并返回 speech probability；`VadDetector::process(prob)` 再按当前检测状态和配置阈值推进状态，并可能产出 `SpeechStart` 或 `AutoStop` 信号。`infer` 和 `process` 均未在名字中标明这条职责链的输入输出领域含义。

## 决定

将模型和 worker 方法统一命名为 `infer_speech_probability(frame)`，将 detector 方法命名为 `observe_probability(speech_probability)`。前者返回模型概率；后者根据概率和现有状态生成状态信号。

## 替代方案

- 保留 `process`：拒绝，名字依赖类型上下文猜测处理对象和副作用。
- 将推理方法命名为 `classify_frame`：拒绝，模型运行的是推理并返回概率；分类状态转移由 detector 完成。

## 影响与验证

- 同步 Input recording loop、worker、模型 API 和 detector 单元测试的调用名。
- VAD 状态机、概率阈值、信号和录音行为不变。
- 验证：Rust `fmt --check`、workspace `check`、严格 Clippy、workspace serial tests、ADR 索引与差异空白检查。

## 回滚

将 `infer_speech_probability` / `observe_probability` 恢复为 `infer` / `process`，并同步还原录音循环、worker、测试与命名文档。
