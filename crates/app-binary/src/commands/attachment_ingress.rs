//! Server-side validation and normalization for uploaded media attachments.

use base64::Engine as _;
use haven_common::config::ContextLimitsConfig;
use haven_common::types::MessageAttachment;

/// Validate the browser payload at the host boundary. Content signatures and
/// known filenames win over browser MIME metadata, while managed-asset fields
/// are always cleared before persistence mints a fresh identity.
pub(super) fn validate_attachments(
    mut attachments: Vec<MessageAttachment>,
    limits: &ContextLimitsConfig,
) -> Result<Vec<MessageAttachment>, String> {
    let max_images = limits.max_attachment_images;
    let max_files = limits.max_attachment_files;
    let max_image_bytes = limits.max_attachment_image_bytes;
    let max_file_bytes = limits.max_attachment_file_bytes;

    for attachment in &mut attachments {
        attachment.asset_id = None;
        attachment.path = None;
        attachment.sha256 = None;
        attachment.size_bytes = None;
        attachment.expires_at = None;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&attachment.data)
            .map_err(|_| "附件数据不是有效的 base64".to_string())?;
        let filename = attachment.filename.as_deref().unwrap_or_default();
        let detected = haven_common::probe_media_with_hint(
            &bytes,
            filename,
            Some(attachment.media_type.as_str()),
        );
        if detected.media_type != haven_common::MediaType::Unknown {
            attachment.media_type = detected.mime_type;
        }
    }

    let images = attachments
        .iter()
        .filter(|attachment| attachment.is_image())
        .count();
    let files = attachments.len().saturating_sub(images);
    if images > max_images {
        return Err(format!("最多支持 {max_images} 张图片"));
    }
    if files > max_files {
        return Err(format!("最多支持 {max_files} 个文件"));
    }
    for attachment in &attachments {
        let (cap, label) = if attachment.is_image() {
            (max_image_bytes, "图片")
        } else {
            if attachment
                .filename
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err("文件附件缺少文件名".to_string());
            }
            (max_file_bytes, "文件")
        };
        let decoded_len = attachment.data.len().saturating_mul(3) / 4;
        if decoded_len > cap {
            return Err(format!("{label}超过 {}MB 上限", cap / 1024 / 1024));
        }
        if base64::engine::general_purpose::STANDARD
            .decode(&attachment.data)
            .is_err()
        {
            return Err("附件数据不是有效的 base64".to_string());
        }
    }
    Ok(attachments)
}
