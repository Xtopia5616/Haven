//! Bounded JavaScript evaluation for the opt-in MCP CodeMode surface.
//!
//! The evaluator is intentionally separate from the model-facing tool catalog.
//! Its only host capability is a caller supplied subcall port; tool names are
//! checked against the per-run allowlist before they cross that port.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const MAX_PENDING_SUBCALLS: usize = 1;
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

struct InterruptOnDrop(Arc<AtomicBool>);

impl Drop for InterruptOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Hard ceilings for one CodeMode invocation. They are process policy, not
/// model-controlled settings; future UI settings may only lower these values.
#[derive(Debug, Clone, Copy)]
pub struct CodeModeLimits {
    pub max_script_bytes: usize,
    pub max_tools: usize,
    pub max_tool_metadata_bytes: usize,
    pub max_calls: usize,
    pub max_subcall_input_bytes: usize,
    pub max_single_result_bytes: usize,
    pub max_total_result_bytes: usize,
    pub max_final_result_bytes: usize,
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub timeout: Duration,
}

impl Default for CodeModeLimits {
    fn default() -> Self {
        Self {
            max_script_bytes: 64 * 1024,
            max_tools: 64,
            max_tool_metadata_bytes: 128 * 1024,
            max_calls: 16,
            max_subcall_input_bytes: 16 * 1024,
            max_single_result_bytes: 16 * 1024,
            max_total_result_bytes: 64 * 1024,
            max_final_result_bytes: 32 * 1024,
            memory_bytes: 8 * 1024 * 1024,
            stack_bytes: 256 * 1024,
            timeout: Duration::from_secs(30),
        }
    }
}

impl CodeModeLimits {
    fn clamp_to_process_ceiling(mut self) -> Self {
        let ceiling = Self::default();
        self.max_script_bytes = self.max_script_bytes.min(ceiling.max_script_bytes);
        self.max_tools = self.max_tools.min(ceiling.max_tools);
        self.max_tool_metadata_bytes = self
            .max_tool_metadata_bytes
            .min(ceiling.max_tool_metadata_bytes);
        self.max_calls = self.max_calls.min(ceiling.max_calls);
        self.max_subcall_input_bytes = self
            .max_subcall_input_bytes
            .min(ceiling.max_subcall_input_bytes);
        self.max_single_result_bytes = self
            .max_single_result_bytes
            .min(ceiling.max_single_result_bytes);
        self.max_total_result_bytes = self
            .max_total_result_bytes
            .min(ceiling.max_total_result_bytes);
        self.max_final_result_bytes = self
            .max_final_result_bytes
            .min(ceiling.max_final_result_bytes);
        self.memory_bytes = self.memory_bytes.min(ceiling.memory_bytes);
        self.stack_bytes = self.stack_bytes.min(ceiling.stack_bytes);
        self.timeout = self.timeout.min(ceiling.timeout);
        self
    }
}

/// A model-visible description for one capability allowed in this script run.
/// It does not carry an adapter, manager handle, proof, or permission grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeModeToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Bounded observation returned from an independently authorized subcall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeModeCallResult {
    pub success: bool,
    pub output: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub truncated: bool,
}

/// One independently authorized nested operation that needs its own user
/// decision. The Agent attaches the parent provider tool identity when it
/// turns this into a durable session confirmation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NestedConfirmationRequest {
    pub call_index: u32,
    pub step_id: String,
    pub tool_name: String,
    pub input: Value,
    pub receipt: crate::ConfirmationReceipt,
    /// Additional user-facing context for a nested confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Optional structured result retained from the attempt that requested
    /// this confirmation, returned alongside the confirmed retry result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_result: Option<Value>,
}

/// The only host capability exposed to CodeMode JavaScript.
///
/// The implementation must resolve the session and authorization context from
/// its trusted caller. `tool_name` and `input` are untrusted script output.
#[async_trait]
pub trait CodeModeSubcallPort: Send + Sync {
    async fn call(
        &self,
        call_index: u32,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<CodeModeCallResult, String>;

    /// A confirmation-required call pauses the enclosing invocation after
    /// returning from the evaluator, even if untrusted script code catches
    /// the bridge error.
    fn pending_confirmation(&self) -> Option<NestedConfirmationRequest> {
        None
    }
}

/// A completed, bounded CodeMode script result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeModeExecution {
    pub output: Value,
    pub call_count: usize,
}

/// Stable errors for budget exhaustion and fail-closed evaluator behavior.
#[derive(Debug, thiserror::Error)]
pub enum CodeModeError {
    #[error("CodeMode script exceeds the {limit}-byte limit")]
    ScriptTooLarge { limit: usize },
    #[error("CodeMode tool description set exceeds the {limit}-tool limit")]
    TooManyTools { limit: usize },
    #[error("CodeMode tool descriptions exceed the {limit}-byte limit")]
    ToolMetadataTooLarge { limit: usize },
    #[error("CodeMode tool names must be non-empty and unique")]
    InvalidToolSet,
    #[error("CodeMode execution was cancelled")]
    Cancelled,
    #[error("CodeMode execution exceeded its time limit")]
    TimedOut,
    #[error("CodeMode exceeded its {limit}-call limit")]
    CallLimit { limit: usize },
    #[error("CodeMode subcall result exceeded its {limit}-byte limit")]
    SingleResultTooLarge { limit: usize },
    #[error("CodeMode accumulated results exceeded its {limit}-byte limit")]
    TotalResultTooLarge { limit: usize },
    #[error("CodeMode final result exceeded its {limit}-byte limit")]
    FinalResultTooLarge { limit: usize },
    #[error("CodeMode subcall is not in the active tool allowlist")]
    ToolNotAllowed,
    #[error("CodeMode subcall input must be a JSON object")]
    InvalidSubcallInput,
    #[error("CodeMode subcall input exceeds the {limit}-byte limit")]
    SubcallInputTooLarge { limit: usize },
    #[error("CodeMode evaluator failed: {0}")]
    Evaluation(String),
    #[error("CodeMode evaluator worker failed")]
    WorkerFailed,
}

/// Evaluates one untrusted script with a fresh QuickJS runtime and explicit
/// resource ceilings. There is no module loader or operating-system API.
#[derive(Debug, Clone)]
pub struct CodeModeEvaluator {
    limits: CodeModeLimits,
}

impl Default for CodeModeEvaluator {
    fn default() -> Self {
        Self::new(CodeModeLimits::default())
    }
}

impl CodeModeEvaluator {
    pub fn new(limits: CodeModeLimits) -> Self {
        Self {
            limits: limits.clamp_to_process_ceiling(),
        }
    }

    pub fn limits(&self) -> CodeModeLimits {
        self.limits
    }

    pub async fn execute(
        &self,
        script: &str,
        tools: Vec<CodeModeToolDefinition>,
        subcalls: Arc<dyn CodeModeSubcallPort>,
        cancel: CancellationToken,
    ) -> Result<CodeModeExecution, CodeModeError> {
        if script.len() > self.limits.max_script_bytes {
            return Err(CodeModeError::ScriptTooLarge {
                limit: self.limits.max_script_bytes,
            });
        }
        if tools.len() > self.limits.max_tools {
            return Err(CodeModeError::TooManyTools {
                limit: self.limits.max_tools,
            });
        }

        let allowed = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<BTreeSet<_>>();
        if allowed.len() != tools.len() || allowed.contains("") {
            return Err(CodeModeError::InvalidToolSet);
        }

        let tool_json = serialize_json_limited(&tools, self.limits.max_tool_metadata_bytes)
            .map_err(|error| match error {
                LimitedSerializationError::LimitExceeded => CodeModeError::ToolMetadataTooLarge {
                    limit: self.limits.max_tool_metadata_bytes,
                },
                LimitedSerializationError::Serialize(error) => CodeModeError::Evaluation(error),
            })?;
        let script = script.to_owned();
        let limits = self.limits;
        let deadline = Instant::now() + limits.timeout;
        let interrupted = Arc::new(AtomicBool::new(false));
        let worker_interrupted = interrupted.clone();
        let _interrupt_on_drop = InterruptOnDrop(interrupted.clone());
        let fatal_reason = Arc::new(AtomicU8::new(0));
        let worker_fatal_reason = fatal_reason.clone();
        let (request_tx, mut request_rx) = mpsc::channel(MAX_PENDING_SUBCALLS);
        let mut worker: JoinHandle<Result<String, CodeModeError>> =
            tokio::task::spawn_blocking(move || {
                evaluate_javascript(
                    &script,
                    &tool_json,
                    EvaluatorSetup {
                        request_tx,
                        limits,
                        deadline,
                        interrupted: worker_interrupted,
                        fatal_reason: worker_fatal_reason,
                    },
                )
            });

        let mut call_count = 0usize;
        let mut total_result_bytes = 0usize;
        loop {
            tokio::select! {
                joined = &mut worker => {
                    if let Some(error) = fatal_error(
                        fatal_reason.load(Ordering::SeqCst),
                        limits.max_subcall_input_bytes,
                    ) {
                        return Err(error);
                    }
                    let serialized = joined.map_err(|_| CodeModeError::WorkerFailed)??;
                    let result_bytes = serialized.len();
                    if result_bytes > limits.max_final_result_bytes {
                        return Err(CodeModeError::FinalResultTooLarge {
                            limit: limits.max_final_result_bytes,
                        });
                    }
                    let output = serde_json::from_str(&serialized)
                        .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
                    return Ok(CodeModeExecution { output, call_count });
                }
                request = request_rx.recv() => {
                    let Some(request) = request else {
                        // The evaluator completed or failed without another
                        // host request. Its JoinHandle branch owns the result.
                        continue;
                    };
                    if !allowed.contains(request.tool_name.as_str()) {
                        fatal_reason.store(2, Ordering::SeqCst);
                        interrupted.store(true, Ordering::SeqCst);
                        let _ = request.reply.send(Err("tool is not allowed in this CodeMode run".into()));
                        let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                        return Err(CodeModeError::ToolNotAllowed);
                    }
                    if call_count >= limits.max_calls {
                        interrupted.store(true, Ordering::SeqCst);
                        let _ = request.reply.send(Err("CodeMode call budget exhausted".into()));
                        let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                        return Err(CodeModeError::CallLimit { limit: limits.max_calls });
                    }
                    call_count += 1;
                    let call_cancel = cancel.child_token();
                    let child_cancel = call_cancel.clone();
                    let call = subcalls.call(
                        call_count.saturating_sub(1) as u32,
                        &request.tool_name,
                        request.input,
                        call_cancel,
                    );
                    let result = tokio::select! {
                        result = call => result,
                        _ = cancel.cancelled() => {
                            child_cancel.cancel();
                            let _ = request.reply.send(Err("CodeMode execution was cancelled".into()));
                            interrupted.store(true, Ordering::SeqCst);
                            let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                            return Err(CodeModeError::Cancelled);
                        }
                        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                            child_cancel.cancel();
                            let _ = request.reply.send(Err("CodeMode execution timed out".into()));
                            interrupted.store(true, Ordering::SeqCst);
                            let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                            return Err(CodeModeError::TimedOut);
                        }
                    };
                    match result {
                        Ok(result) => {
                            let serialized = match serialize_json_limited(
                                &result,
                                limits.max_single_result_bytes,
                            ) {
                                Ok(serialized) => serialized,
                                Err(LimitedSerializationError::Serialize(error)) => {
                                    let _ = request.reply.send(Err("subcall result serialization failed".into()));
                                    interrupted.store(true, Ordering::SeqCst);
                                    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                                    return Err(CodeModeError::Evaluation(error));
                                }
                                Err(LimitedSerializationError::LimitExceeded) => {
                                    interrupted.store(true, Ordering::SeqCst);
                                    let _ = request.reply.send(Err("subcall result budget exhausted".into()));
                                    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                                    return Err(CodeModeError::SingleResultTooLarge {
                                        limit: limits.max_single_result_bytes,
                                    });
                                }
                            };
                            let result_bytes = serialized.len();
                            total_result_bytes = total_result_bytes.saturating_add(result_bytes);
                            if total_result_bytes > limits.max_total_result_bytes {
                                interrupted.store(true, Ordering::SeqCst);
                                let _ = request.reply.send(Err("cumulative result budget exhausted".into()));
                                let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                                return Err(CodeModeError::TotalResultTooLarge {
                                    limit: limits.max_total_result_bytes,
                                });
                            }
                            let _ = request.reply.send(Ok(serialized));
                        }
                        Err(error) => {
                            let _ = request.reply.send(Err(error));
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    interrupted.store(true, Ordering::SeqCst);
                    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                    return Err(CodeModeError::Cancelled);
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    interrupted.store(true, Ordering::SeqCst);
                    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, &mut worker).await;
                    return Err(CodeModeError::TimedOut);
                }
            }
        }
    }
}

struct SubcallRequest {
    tool_name: String,
    input: Value,
    reply: oneshot::Sender<Result<String, String>>,
}

struct EvaluatorSetup {
    request_tx: mpsc::Sender<SubcallRequest>,
    limits: CodeModeLimits,
    deadline: Instant,
    interrupted: Arc<AtomicBool>,
    fatal_reason: Arc<AtomicU8>,
}

#[derive(Debug)]
enum LimitedSerializationError {
    LimitExceeded,
    Serialize(String),
}

struct LimitedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for LimitedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "JSON byte limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_json_limited<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<String, LimitedSerializationError> {
    let mut writer = LimitedJsonWriter {
        bytes: Vec::with_capacity(limit.min(4096)),
        limit,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.exceeded {
            return Err(LimitedSerializationError::LimitExceeded);
        }
        return Err(LimitedSerializationError::Serialize(error.to_string()));
    }
    String::from_utf8(writer.bytes)
        .map_err(|error| LimitedSerializationError::Serialize(error.to_string()))
}

fn evaluate_javascript(
    script: &str,
    tools_json: &str,
    setup: EvaluatorSetup,
) -> Result<String, CodeModeError> {
    use rquickjs::{Context, Exception, Function, Runtime, Value as JsValue};

    let EvaluatorSetup {
        request_tx,
        limits,
        deadline,
        interrupted,
        fatal_reason,
    } = setup;

    let runtime = Runtime::new().map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
    runtime.set_memory_limit(limits.memory_bytes);
    runtime.set_max_stack_size(limits.stack_bytes);
    let handler_interrupted = interrupted.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        handler_interrupted.load(Ordering::SeqCst) || Instant::now() >= deadline
    })));

    let context =
        Context::full(&runtime).map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
    let result = context.with(|ctx| {
        let globals = ctx.globals();
        let sender = request_tx.clone();
        let callback_fatal_reason = fatal_reason.clone();
        let max_input_bytes = limits.max_subcall_input_bytes;
        let host_call = Function::new(
            ctx.clone(),
            move |ctx: rquickjs::Ctx<'_>, tool_name: String, input_json: String| {
                if input_json.len() > max_input_bytes {
                    callback_fatal_reason.store(3, Ordering::SeqCst);
                    return Err(Exception::throw_range(
                        &ctx,
                        "CodeMode subcall input exceeds its byte limit",
                    ));
                }
                let input: Value = serde_json::from_str(&input_json)
                    .map_err(|_| {
                        callback_fatal_reason.store(1, Ordering::SeqCst);
                        Exception::throw_type(&ctx, "CodeMode subcall input must be JSON")
                    })?;
                if !input.is_object() {
                    callback_fatal_reason.store(1, Ordering::SeqCst);
                    return Err(Exception::throw_type(
                        &ctx,
                        "CodeMode subcall input must be a JSON object",
                    ));
                }
                let (reply, response) = oneshot::channel();
                sender
                    .blocking_send(SubcallRequest {
                        tool_name,
                        input,
                        reply,
                    })
                    .map_err(|_| Exception::throw_type(&ctx, "CodeMode host call was cancelled"))?;
                let serialized = response
                    .blocking_recv()
                    .map_err(|_| Exception::throw_type(&ctx, "CodeMode host call was cancelled"))?
                    .map_err(|error| Exception::throw_message(&ctx, &error))?;
                Ok(serialized)
            },
        )
        .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
        globals
            .set("__haven_codemode_call", host_call)
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
        globals
            .set("__haven_codemode_tools", tools_json)
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;

        // QuickJS is built without the loader feature. The global allowlist
        // removes time, dynamic-code, process, network and file-facing APIs;
        // only the structured `tools` bridge remains as a host capability.
        // Remove prototype-level dynamic constructors while the built-in
        // Function binding still exists inside this trusted bootstrap.
        ctx.eval::<(), _>(r#"
            for (const prototype of [
                Function.prototype,
                Object.getPrototypeOf(async function() {}),
                Object.getPrototypeOf(function*() {}),
                Object.getPrototypeOf(async function*() {})
            ]) {
                Object.defineProperty(prototype, "constructor", {
                    value: undefined,
                    writable: false,
                    configurable: false
                });
            }
        "#)
        .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;

        for name in [
            "Date",
            "Deno",
            "Bun",
            "process",
            "require",
            "std",
            "os",
            "fetch",
            "XMLHttpRequest",
            "WebSocket",
            "setTimeout",
            "setInterval",
            "clearTimeout",
            "queueMicrotask",
            "performance",
            "eval",
            "Function",
            "WebAssembly",
        ] {
            globals
                .set(name, JsValue::new_undefined(ctx.clone()))
                .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
        }

        let bootstrap = r#"
            const deepFreeze = value => {
                if (value && typeof value === "object" && !Object.isFrozen(value)) {
                    for (const key of Object.keys(value)) deepFreeze(value[key]);
                    Object.freeze(value);
                }
                return value;
            };
            const definitions = deepFreeze(JSON.parse(__haven_codemode_tools));
            const invokeSubcall = globalThis.__haven_codemode_call;
            const call = (name, input = {}) => {
                const serialized = JSON.stringify(input);
                if (typeof serialized !== "string") throw new TypeError("tool input must be JSON serializable");
                return JSON.parse(invokeSubcall(name, serialized));
            };
            Object.defineProperty(globalThis, "tools", {
                value: Object.freeze({ available: definitions, call }),
                configurable: false,
                enumerable: true,
                writable: false
            });
            delete globalThis.__haven_codemode_call;
            delete globalThis.__haven_codemode_tools;
        "#;
        ctx.eval::<(), _>(bootstrap)
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;

        let wrapped_script = format!("(async () => {{\n{script}\n}})()");
        let promise: rquickjs::Promise<'_> = ctx
            .eval(wrapped_script)
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
        let value: JsValue = promise
            .finish()
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
        let serialized = ctx
            .json_stringify(value)
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?
            .map(|value| value.to_string())
            .unwrap_or(Ok("null".to_owned()))
            .map_err(|error| CodeModeError::Evaluation(error.to_string()))?;
        Ok(serialized)
    });

    if Instant::now() >= deadline {
        return Err(CodeModeError::TimedOut);
    }
    if let Some(error) = fatal_error(
        fatal_reason.load(Ordering::SeqCst),
        limits.max_subcall_input_bytes,
    ) {
        return Err(error);
    }
    if interrupted.load(Ordering::SeqCst) {
        return Err(CodeModeError::Cancelled);
    }
    result
}

fn fatal_error(code: u8, max_subcall_input_bytes: usize) -> Option<CodeModeError> {
    match code {
        1 => Some(CodeModeError::InvalidSubcallInput),
        2 => Some(CodeModeError::ToolNotAllowed),
        3 => Some(CodeModeError::SubcallInputTooLarge {
            limit: max_subcall_input_bytes,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Mutex, Notify};

    struct EchoSubcalls {
        calls: Mutex<Vec<(String, Value)>>,
        failures: AtomicUsize,
    }

    struct BlockingSubcalls {
        started: Notify,
    }

    #[async_trait]
    impl CodeModeSubcallPort for BlockingSubcalls {
        async fn call(
            &self,
            _call_index: u32,
            _tool_name: &str,
            _input: Value,
            cancel: CancellationToken,
        ) -> Result<CodeModeCallResult, String> {
            self.started.notify_one();
            cancel.cancelled().await;
            Err("subcall cancelled".into())
        }
    }

    #[async_trait]
    impl CodeModeSubcallPort for EchoSubcalls {
        async fn call(
            &self,
            _call_index: u32,
            tool_name: &str,
            input: Value,
            _cancel: CancellationToken,
        ) -> Result<CodeModeCallResult, String> {
            self.calls
                .lock()
                .await
                .push((tool_name.to_owned(), input.clone()));
            if self.failures.load(Ordering::SeqCst) > 0 {
                return Err("test bridge failure".into());
            }
            Ok(CodeModeCallResult {
                success: true,
                output: input,
                error: None,
                truncated: false,
            })
        }
    }

    fn definition(name: &str) -> CodeModeToolDefinition {
        CodeModeToolDefinition {
            name: name.into(),
            description: "bounded test capability".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn bridge() -> Arc<EchoSubcalls> {
        Arc::new(EchoSubcalls {
            calls: Mutex::new(Vec::new()),
            failures: AtomicUsize::new(0),
        })
    }

    #[tokio::test]
    async fn script_composes_sequential_subcalls_through_the_host_port() {
        let bridge = bridge();
        let output = CodeModeEvaluator::default()
            .execute(
                r#"
                    const first = await tools.call("mcp__alpha__echo", {value: 2});
                    const second = tools.call("mcp__alpha__echo", {value: first.output.value + 3});
                    return {value: second.output.value, available: tools.available.length};
                "#,
                vec![definition("mcp__alpha__echo")],
                bridge.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(output.output["value"], 5);
        assert_eq!(output.output["available"], 1);
        assert_eq!(output.call_count, 2);
        assert_eq!(bridge.calls.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn script_cannot_call_a_tool_outside_its_run_allowlist() {
        let bridge = bridge();
        let result = CodeModeEvaluator::default()
            .execute(
                r#"return tools.call("mcp__other__write", {});"#,
                vec![definition("mcp__alpha__echo")],
                bridge.clone(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(CodeModeError::ToolNotAllowed)));
        assert!(bridge.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn host_process_files_network_clock_and_dynamic_code_apis_are_absent() {
        let output = CodeModeEvaluator::default()
            .execute(
                r#"
                    return [typeof process, typeof require, typeof std, typeof os,
                        typeof fetch, typeof XMLHttpRequest, typeof WebSocket,
                        typeof Deno, typeof Bun, typeof setTimeout, typeof setInterval,
                        typeof clearTimeout, typeof queueMicrotask, typeof performance,
                        typeof Date, typeof eval, typeof Function, typeof WebAssembly,
                        typeof ({}).constructor.constructor,
                        typeof (async function() {}).constructor,
                        typeof (function*() {}).constructor,
                        typeof (async function*() {}).constructor,
                        typeof tools.call.constructor];
                "#,
                Vec::new(),
                bridge(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.output, serde_json::json!(vec!["undefined"; 23]));
    }

    #[tokio::test]
    async fn script_byte_limit_fails_before_creating_the_runtime() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_script_bytes: 8,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                "return 'too long';",
                Vec::new(),
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CodeModeError::ScriptTooLarge { limit: 8 })
        ));
    }

    #[tokio::test]
    async fn tool_metadata_size_is_bounded_before_runtime_creation() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_tool_metadata_bytes: 128,
            ..CodeModeLimits::default()
        });
        let mut tool = definition("mcp__alpha__echo");
        tool.description = "x".repeat(1024);
        let result = evaluator
            .execute(
                "return null;",
                vec![tool],
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CodeModeError::ToolMetadataTooLarge { limit: 128 })
        ));
    }

    #[test]
    fn caller_cannot_raise_process_resource_ceilings() {
        let limits = CodeModeEvaluator::new(CodeModeLimits {
            max_calls: usize::MAX,
            memory_bytes: usize::MAX,
            timeout: Duration::from_secs(24 * 60 * 60),
            ..CodeModeLimits::default()
        })
        .limits();
        assert_eq!(limits.max_calls, 16);
        assert_eq!(limits.memory_bytes, 8 * 1024 * 1024);
        assert_eq!(limits.timeout, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn execution_timeout_interrupts_an_infinite_loop() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            timeout: Duration::from_millis(100),
            ..CodeModeLimits::default()
        });
        let started = Instant::now();
        let result = evaluator
            .execute(
                "while (true) {}",
                Vec::new(),
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(CodeModeError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn call_and_observation_budgets_stop_before_more_host_calls() {
        let bridge = bridge();
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_calls: 1,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                r#"
                    tools.call("mcp__alpha__echo", {value: 1});
                    tools.call("mcp__alpha__echo", {value: 2});
                    return null;
                "#,
                vec![definition("mcp__alpha__echo")],
                bridge.clone(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(CodeModeError::CallLimit { limit: 1 })));
        assert_eq!(bridge.calls.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn serialized_subcall_input_has_an_independent_byte_limit() {
        let bridge = bridge();
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_subcall_input_bytes: 128,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                r#"return tools.call("mcp__alpha__echo", {value: "x".repeat(1024)});"#,
                vec![definition("mcp__alpha__echo")],
                bridge.clone(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CodeModeError::SubcallInputTooLarge { limit: 128 })
        ));
        assert!(bridge.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn single_subcall_result_has_an_independent_byte_limit() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_single_result_bytes: 128,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                r#"return tools.call("mcp__alpha__echo", {value: "x".repeat(100)});"#,
                vec![definition("mcp__alpha__echo")],
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CodeModeError::SingleResultTooLarge { limit: 128 })
        ));
    }

    #[tokio::test]
    async fn accumulated_subcall_results_have_a_combined_byte_limit() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_total_result_bytes: 200,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                r#"
                    tools.call("mcp__alpha__echo", {value: "x".repeat(60)});
                    tools.call("mcp__alpha__echo", {value: "y".repeat(60)});
                    return null;
                "#,
                vec![definition("mcp__alpha__echo")],
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CodeModeError::TotalResultTooLarge { limit: 200 })
        ));
    }

    #[tokio::test]
    async fn final_script_result_has_its_own_byte_limit() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            max_final_result_bytes: 16,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                r#"return "x".repeat(32);"#,
                Vec::new(),
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CodeModeError::FinalResultTooLarge { limit: 16 })
        ));
    }

    #[tokio::test]
    async fn caller_cancellation_interrupts_an_active_authorized_subcall() {
        let cancel = CancellationToken::new();
        let bridge = Arc::new(BlockingSubcalls {
            started: Notify::new(),
        });
        let evaluator = CodeModeEvaluator::default();
        let run = tokio::spawn({
            let cancel = cancel.clone();
            let bridge = bridge.clone();
            async move {
                evaluator
                    .execute(
                        r#"return tools.call("mcp__alpha__wait", {});"#,
                        vec![definition("mcp__alpha__wait")],
                        bridge,
                        cancel,
                    )
                    .await
            }
        });

        bridge.started.notified().await;
        cancel.cancel();
        let result = run.await.unwrap();
        assert!(matches!(result, Err(CodeModeError::Cancelled)));
    }

    #[tokio::test]
    async fn runtime_memory_limit_rejects_a_large_allocation() {
        let evaluator = CodeModeEvaluator::new(CodeModeLimits {
            memory_bytes: 2 * 1024 * 1024,
            ..CodeModeLimits::default()
        });
        let result = evaluator
            .execute(
                "return new ArrayBuffer(32 * 1024 * 1024);",
                Vec::new(),
                bridge(),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(CodeModeError::Evaluation(_))));
    }
}
