//! Multi-modal media gateway: the "模态检测 → 意图分类 → 路由 → 统一输出"
//! pipeline as an in-process crate layered on `haven-llm`.
//!
//! Pipeline stages:
//! 1. [`modality::detect_modality`] — zero-cost rule-based detection
//!    (magic bytes → extension fallback → text decoding fallback).
//! 2. [`intent::detect_intent`] — keyword-rule classification into
//!    extract / understand / generate.
//! 3. [`coverage`] — the (modality × intent) coverage table: only the
//!    high-cost extraction actions (OCR / ASR) get dedicated providers, the
//!    rest pass through to the main model.
//! 4. [`gateway::MediaGateway`] — orchestration: confidence gating
//!    (specialized result below threshold → main model) and error fallback
//!    (specialized failure → main model).
//!
//! Integration point: the agent calls [`MediaGateway::process_attachment`]
//! when a user message carries binary attachments, and
//! [`MediaGateway::process_generate`] when the user text asks for media
//! generation (TTS / text-to-image).

pub mod coverage;
pub mod gateway;
pub mod intent;
pub mod modality;

pub use coverage::{CoverageAction, MediaDecision};
pub use gateway::{AttachmentOutcome, GenerateOutcome, MediaGateway};
pub use intent::{Intent, detect_intent};
pub use modality::{Modality, detect_modality};
