//! Stage 2: intent classification — user instruction > UI override > rules.
//!
//! Three intents suffice (spec):
//! - **extract**: information already exists in the input, take it out
//!   faithfully (OCR / ASR / subtitles / translation).
//! - **understand**: the model must reason (description / Q&A / summary /
//!   analysis).
//! - **generate**: the output is another modality (text-to-image).
//!
//! Classification is keyword-rule based (zero cost); the rule set is a
//! denylist-free positive match — extract keywords win over generate
//! keywords when both appear ("把图片文字提取出来生成报告" is still
//! extract), and everything unmatched defaults to understand.

/// Classified intent of the user's instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    Extract,
    Understand,
    Generate,
}

impl Intent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Intent::Extract => "extract",
            Intent::Understand => "understand",
            Intent::Generate => "generate",
        }
    }
}

/// Extract-intent keywords (信息在输入里 → 忠实取出).
const EXTRACT_KEYWORDS: &[&str] = &[
    "提取",
    "识别",
    "转文字",
    "转文本",
    "转写",
    "字幕",
    "抄录",
    "翻译",
    "ocr",
    "asr",
    "转录",
    "文字内容",
];

/// Generate-intent keywords (输出另一种模态). Deliberately narrow: bare
/// "生成/制作/合成" ("生成报告"、"制作表格"、"合成音乐") stays with the
/// agent, and 图片/图像 alone ("这张图片里有什么") must not trigger
/// generation. Only explicit media-generation phrasing routes here.
const IMAGE_GEN_KEYWORDS: &[&str] = &["画", "绘制", "文生图", "海报", "插画", "生成图", "logo"];

/// Classify the intent from user text. `explicit` (UI-provided override)
/// wins when present; otherwise the keyword rules apply, defaulting to
/// understand.
pub fn detect_intent(user_text: &str, explicit: Option<Intent>) -> Intent {
    if let Some(intent) = explicit {
        return intent;
    }
    let lower = user_text.to_ascii_lowercase();
    // Extract wins over generate so compound requests ("提取文字并生成报告")
    // stay faithful to the input data.
    if EXTRACT_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return Intent::Extract;
    }
    if is_generate_text(&lower) {
        return Intent::Generate;
    }
    Intent::Understand
}

/// Whether the text asks for image generation.
fn is_generate_text(lower: &str) -> bool {
    IMAGE_GEN_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// The only automatic media-generation capability is text-to-image. Speech is
/// deliberately model-tool driven through `audio(operation="speak")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateKind {
    Image,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keywords() {
        for text in [
            "提取这张图片的文字",
            "帮我识别图片里的内容",
            "把这段音频转文字",
            "音频转写",
            "生成字幕",
            "抄录一下",
            "翻译这段文字",
            "OCR 这张图",
            "图片里的文字内容",
        ] {
            assert_eq!(detect_intent(text, None), Intent::Extract, "{text}");
        }
    }

    #[test]
    fn generate_keywords() {
        for text in ["画一只猫", "生成一张海报", "帮我画一个插画", "文生图：星空"]
        {
            assert_eq!(detect_intent(text, None), Intent::Generate, "{text}");
        }
    }

    #[test]
    fn ambiguous_verbs_do_not_trigger_generation() {
        // 生成/制作/合成 alone (报告、表格、代码) stays with the agent.
        for text in [
            "生成一个报告",
            "帮我制作一个表格",
            "生成代码",
            "合成音乐",
            "生成一个视频",
        ] {
            assert_eq!(detect_intent(text, None), Intent::Understand, "{text}");
        }
        // 图片/图像 alone is a reference, not a generation request.
        for text in ["这张图片里有什么", "图像处理一下", "帮我看看照片"] {
            assert_eq!(detect_intent(text, None), Intent::Understand, "{text}");
        }
    }

    #[test]
    fn understand_default() {
        for text in [
            "这段音乐什么风格",
            "帮我分析一下",
            "这张图片讲了什么",
            "总结一下这个文档",
            "你好",
            "abc def",
        ] {
            assert_eq!(detect_intent(text, None), Intent::Understand, "{text}");
        }
    }

    #[test]
    fn extract_wins_over_generate_in_compound_request() {
        assert_eq!(
            detect_intent("把图片文字提取出来，生成一个报告", None),
            Intent::Extract
        );
    }

    #[test]
    fn explicit_override_wins() {
        assert_eq!(
            detect_intent("随便说说", Some(Intent::Extract)),
            Intent::Extract
        );
        assert_eq!(
            detect_intent("提取文字", Some(Intent::Understand)),
            Intent::Understand
        );
    }

    #[test]
    fn speech_requests_stay_with_the_agent() {
        for text in ["朗读这段文字", "把这段话读出来", "配音这段文字", "唱一首歌"]
        {
            assert_eq!(detect_intent(text, None), Intent::Understand, "{text}");
        }
    }
}
