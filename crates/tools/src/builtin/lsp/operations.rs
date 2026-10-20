//! LSP operation schemas, model guidance, and security metadata.

use crate::{
    ConfirmationRequirement, DataSensitivity, ExecutionBoundary, NetworkAccess, OperationEffect,
    OperationIdempotency, OperationPolicy, OperationSpec, ToolConcurrency, ToolOperationScope,
};
use haven_common::tools::{ToolCatalogGroup, ToolPresentation, ToolPrompt, ToolSource};
use haven_common::types::{CapabilityScope, RiskLevel};
use serde_json::{Value, json};

pub(in crate::builtin) fn operation_specs() -> Vec<OperationSpec> {
    [
        (
            "lsp.definition",
            "definition",
            position_schema(false),
            "代码定义",
        ),
        (
            "lsp.implementation",
            "implementation",
            position_schema(false),
            "代码实现",
        ),
        (
            "lsp.type_definition",
            "type_definition",
            position_schema(false),
            "类型定义",
        ),
        (
            "lsp.references",
            "references",
            position_schema(true),
            "代码引用",
        ),
        ("lsp.hover", "hover", position_schema(false), "代码悬停信息"),
        (
            "lsp.document_symbols",
            "document_symbols",
            document_schema(),
            "文档符号",
        ),
        (
            "lsp.workspace_symbols",
            "workspace_symbols",
            workspace_symbols_schema(),
            "工作区符号搜索",
        ),
        (
            "lsp.diagnostics",
            "diagnostics",
            document_schema(),
            "代码诊断",
        ),
    ]
    .into_iter()
    .map(|(name, operation, schema, label)| {
        let contract = super::super::operation_contract(name);
        let capability = CapabilityScope::try_new(name).expect("LSP operation name is canonical");
        OperationSpec {
            name: name.into(),
            description: crate::prompts::operation_text(name).description.into(),
            fixed: vec![("operation".into(), json!(operation))],
            schema,
            policy: OperationPolicy {
                risk_level: contract.risk_override.unwrap_or(RiskLevel::High),
                capability,
                confirmation: ConfirmationRequirement::SecurityPolicy,
                idempotency: OperationIdempotency::NonIdempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::Exclusive,
                effect: OperationEffect::ExternalEffect,
                data_sensitivity: DataSensitivity::UserData,
                network_access: NetworkAccess::Opaque,
                execution_boundary: ExecutionBoundary::HostProcess,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::Files,
            presentation: ToolPresentation {
                label: label.into(),
                renderer: "lsp".into(),
                icon: "code".into(),
                represented_source: ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: crate::prompts::operation_text(name).when_to_use.into(),
                when_not_to_use: String::new(),
                key_operations: vec![name.into()],
            },
            identity: None,
        }
    })
    .collect()
}

fn document_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{"path":{"type":"string","minLength":1,"description":"Workspace-relative source file path; absolute paths are limited to the active execution workspace"}},
        "required":["path"]
    })
}

fn workspace_symbols_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "properties":{
            "server":{"type":"string","minLength":1,"maxLength":128,"description":"Name of an enabled LSP server from lsp_servers in config.toml"},
            "query":{"type":"string","minLength":1,"maxLength":256,"description":"Symbol name or substring to search for in the configured language server's workspace index"}
        },
        "required":["server","query"]
    })
}

fn position_schema(include_declaration: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("path".into(), json!({"type":"string","minLength":1,"description":"Workspace-relative source file path; absolute paths are limited to the active execution workspace"}));
    properties.insert(
        "line".into(),
        json!({"type":"integer","minimum":1,"description":"1-based source line"}),
    );
    properties.insert("character".into(), json!({"type":"integer","minimum":1,"description":"1-based UTF-16 code-unit column, as used by LSP position fields"}));
    if include_declaration {
        properties.insert("include_declaration".into(), json!({"type":"boolean","default":true,"description":"Whether reference results include the declaration"}));
    }
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":["path","line","character"]})
}
