const REPRESENTATION_LABELS: Record<string, string> = {
	raw_image: '原始图片',
	raw_audio: '原始音频',
	raw_video: '原始视频',
	extracted_text: '提取文本',
	transcript: '转写文本',
	ocr_text: 'OCR 文本',
	image_description: '图片描述',
	document_pages: '文档页',
	table_data: '表格数据',
	thumbnail: '缩略图',
	managed_file_ref: '受管文件引用',
};

const STRATEGY_LABELS: Record<string, string> = {
	auto: '自动',
	raw_preferred: '优先原始媒体',
	extracted_preferred: '优先派生内容',
	text_only_safe: '仅安全文本',
};

const NOTICE_LABELS: Record<string, string> = {
	raw_capability_unsupported: '当前模型不支持原始媒体类型',
	raw_capability_unknown: '当前模型能力未知，未发送原始媒体',
	raw_mime_unsupported: '媒体格式不受当前模型支持',
	raw_size_exceeded: '媒体超过当前模型输入上限',
	input_part_limit: '媒体数量超过当前模型上限',
	no_compatible_representation: '没有兼容的附件表示',
	strategy_excluded: '当前媒体策略排除了原始附件',
	derived_fallback: '已改用可用的派生内容',
	managed_reference_fallback: '已改用受管附件引用',
};

export function mediaRepresentationLabel(representation: string): string {
	return REPRESENTATION_LABELS[representation] || representation || '未知表示';
}

export function mediaPlanStrategyLabel(strategy: string): string {
	return STRATEGY_LABELS[strategy] || strategy || '自动';
}

export function mediaPlanNoticeLabel(code: string): string {
	return NOTICE_LABELS[code] || '附件表示已调整';
}

export function mediaPlanProjectionLabel(projection: {
	assetId: string;
	representation: string;
	mode: string;
}): string {
	const mode = projection.mode === 'derived' ? '（派生）' : '';
	return `${projection.assetId}：${mediaRepresentationLabel(projection.representation)}${mode}`;
}
