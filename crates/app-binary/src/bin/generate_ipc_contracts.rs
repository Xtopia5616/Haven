use quote::ToTokens;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::{FnArg, Item, Pat, ReturnType, Type};

#[path = "../ipc_codegen_types.rs"]
mod ipc_types;
use ipc_types::{RustTypeGraph, TypeContext, TypeUse};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Command {
    name: String,
    request: String,
    response: String,
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("ui/src/lib/contracts/generatedCommands.ts");
    let generated =
        generate(&root).unwrap_or_else(|error| panic!("IPC contract generation failed: {error}"));

    match std::env::args().nth(1).as_deref() {
        Some("--check") => {
            let current = fs::read_to_string(&output).unwrap_or_else(|_| {
                panic!(
                    "{} is missing; run scripts/generate-ipc-contracts.ps1",
                    output.display()
                )
            });
            if current != generated {
                panic!(
                    "{} is stale; run scripts/generate-ipc-contracts.ps1",
                    output.display()
                );
            }
        }
        Some("--write") | None => {
            fs::write(&output, generated)
                .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
        }
        Some(argument) => panic!("unsupported argument {argument:?}; expected --check or --write"),
    }
}

fn generate(root: &Path) -> Result<String, String> {
    let commands_root = root.join("crates/app-binary/src/commands");
    let mut type_graph = RustTypeGraph::load(root)?;
    let mut commands = BTreeMap::new();
    let mut files = fs::read_dir(&commands_root)
        .map_err(|error| format!("read {}: {error}", commands_root.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("list command files: {error}"))?;
    files.sort_by_key(|entry| entry.file_name());

    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let file = syn::parse_file(&source)
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        let context = RustTypeGraph::context_for_command(root, &path, &file.items)?;
        for item in &file.items {
            let Item::Fn(function) = item else { continue };
            if !is_tauri_command(&function.attrs)
                || !matches!(function.vis, syn::Visibility::Public(_))
            {
                continue;
            }
            let name = function.sig.ident.to_string();
            let request = request_type(
                &function.sig.inputs,
                argument_case(&function.attrs)?,
                &mut type_graph,
                &context,
            )
            .map_err(|error| format!("command {name} request: {error}"))?;
            let response = response_type(&function.sig.output, &mut type_graph, &context)
                .map_err(|error| format!("command {name} response: {error}"))?;
            let command = Command {
                name: name.clone(),
                request,
                response,
            };
            if commands.insert(name.clone(), command).is_some() {
                return Err(format!("command {name} is declared more than once"));
            }
        }
    }

    if commands.is_empty() {
        return Err("no #[tauri::command] handlers found".into());
    }

    // Session lifecycle events are public IPC contracts even though event
    // payloads are not returned from Tauri commands. Keep their tagged union
    // generated from the same Rust Serde authority as command responses.
    type_graph.emit_definition(
        "haven_app_binary_lib::events::SessionLifecycleEvent",
        TypeUse::Response,
    )?;
    type_graph.emit_definition(
        "haven_app_binary_lib::events::VadStatusEvent",
        TypeUse::Response,
    )?;
    for event_type in [
        "AppBootstrapEvent",
        "TrayStatusChangedEvent",
        "MuteChangedEvent",
        "McpStatusChangedEvent",
        "SkillsStatusChangedEvent",
        "HotkeyConflictEvent",
        "HotkeyRebindEvent",
    ] {
        type_graph.emit_definition(
            &format!("haven_app_binary_lib::events::{event_type}"),
            TypeUse::Response,
        )?;
    }
    type_graph.emit_definition("haven_mcp::protocol::McpClientStatus", TypeUse::Response)?;
    type_graph.emit_external_unit_variant_values(
        "haven_mcp::protocol::McpClientStatus",
        "MCP_CLIENT_STATUS_UNIT_VALUES",
        TypeUse::Response,
    )?;

    let mut output = String::from(
        "// Generated from #[tauri::command] handler signatures by `scripts/generate-ipc-contracts.ps1`.\n\
         // Do not edit by hand; `scripts/check-ipc-contracts.ps1` rejects drift.\n\n\
         // DTO declarations below are generated from Rust Serialize types.\n\n",
    );
    output.push_str(&type_graph.declarations());
    output.push_str("\nexport interface TauriCommandMap {\n");
    for command in commands.values() {
        output.push_str(&format!(
            "\t{}: {{ request: {}; response: {} }};\n",
            command.name, command.request, command.response
        ));
    }
    output.push_str(
        "}\n\n\
         export type TauriCommandName = keyof TauriCommandMap;\n\
         export type TauriCommandRequest<K extends TauriCommandName> = TauriCommandMap[K]['request'];\n\
         export type TauriCommandResponse<K extends TauriCommandName> = TauriCommandMap[K]['response'];\n\
         export type TauriCommandRequestArgs<K extends TauriCommandName> = TauriCommandRequest<K> extends undefined\n\
         \t? [request?: undefined]\n\
         \t: {} extends TauriCommandRequest<K>\n\
         \t\t? [request?: TauriCommandRequest<K>]\n\
         \t\t: [request: TauriCommandRequest<K>];\n\n\
         export type TauriCommandInvoke = {\n\
             <K extends TauriCommandName>(\n\
             command: K,\n\
             ...args: TauriCommandRequestArgs<K>\n\
             ): Promise<TauriCommandResponse<K>>;\n\
         };\n",
    );
    Ok(output)
}

fn is_tauri_command(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "command")
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgumentCase {
    Camel,
    Snake,
}

fn argument_case(attributes: &[syn::Attribute]) -> Result<ArgumentCase, String> {
    let Some(command_attribute) = attributes.iter().find(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "command")
    }) else {
        return Err("command attribute missing".into());
    };
    let mut selected = None;
    let syn::Meta::List(_) = &command_attribute.meta else {
        return Ok(ArgumentCase::Camel);
    };
    command_attribute
        .parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                let value = meta.value()?.parse::<syn::LitStr>()?.value();
                selected = Some(match value.as_str() {
                    "camelCase" => ArgumentCase::Camel,
                    "snake_case" => ArgumentCase::Snake,
                    other => {
                        return Err(
                            meta.error(format!("unsupported Tauri rename_all value {other:?}"))
                        );
                    }
                });
            } else {
                return Err(meta.error("unsupported Tauri command attribute argument"));
            }
            Ok(())
        })
        .map_err(|error| format!("invalid Tauri command attribute: {error}"))?;
    Ok(selected.unwrap_or(ArgumentCase::Camel))
}

fn request_type(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
    argument_case: ArgumentCase,
    type_graph: &mut RustTypeGraph,
    context: &TypeContext,
) -> Result<String, String> {
    let mut fields = Vec::new();
    for input in inputs {
        let FnArg::Typed(argument) = input else {
            return Err("receiver arguments are not supported in a Tauri command".into());
        };
        if is_runtime_injected(&argument.ty) {
            continue;
        }
        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(format!(
                "unsupported command argument pattern: {}",
                argument.pat.to_token_stream()
            ));
        };
        let name = match argument_case {
            ArgumentCase::Camel => snake_to_camel(&pattern.ident.to_string()),
            ArgumentCase::Snake => pattern.ident.to_string(),
        };
        let mapped = type_graph.map_type(&argument.ty, TypeUse::Request, context)?;
        let optional = if mapped.optional { "?" } else { "" };
        fields.push(format!("{name}{optional}: {}", mapped.ts));
    }
    if fields.is_empty() {
        Ok("undefined".into())
    } else {
        Ok(format!("{{ {} }}", fields.join("; ")))
    }
}

fn response_type(
    output: &ReturnType,
    type_graph: &mut RustTypeGraph,
    context: &TypeContext,
) -> Result<String, String> {
    let ReturnType::Type(_, ty) = output else {
        return Ok("void".into());
    };
    let Type::Path(path) = ty.as_ref() else {
        return Err(format!(
            "unsupported handler response type: {}",
            ty.to_token_stream()
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err("handler response has an empty type path".into());
    };
    if segment.ident != "Result" {
        return Err(format!(
            "Tauri handler responses must use Result<T, String>; found {}",
            ty.to_token_stream()
        ));
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err("Result response is missing type arguments".into());
    };
    let mut args = arguments.args.iter();
    let Some(syn::GenericArgument::Type(success)) = args.next() else {
        return Err("Result response is missing success type".into());
    };
    let Some(syn::GenericArgument::Type(error)) = args.next() else {
        return Err("Result response is missing error type".into());
    };
    if !matches!(error, Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "String"))
    {
        return Err(format!(
            "Tauri command error must be String, found {}",
            error.to_token_stream()
        ));
    }
    Ok(type_graph.map_type(success, TypeUse::Response, context)?.ts)
}

fn is_runtime_injected(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    path.path.segments.last().is_some_and(|segment| {
        matches!(
            segment.ident.to_string().as_str(),
            "State" | "AppHandle" | "Window" | "WebviewWindow"
        )
    })
}

fn snake_to_camel(value: &str) -> String {
    let mut parts = value.split('_');
    let mut result = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.extend(first.to_uppercase());
            result.extend(chars);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_contract_comes_from_flat_handler_arguments_and_omits_tauri_state() {
        let source = r#"
            #[tauri::command]
            async fn update_title(state: State<'_, AppState>, session_id: String, pause: Option<bool>) -> Result<(), String> { todo!() }
        "#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert_eq!(
            request_type(
                &function.sig.inputs,
                ArgumentCase::Camel,
                &mut graph,
                &context
            )
            .unwrap(),
            "{ sessionId: string; pause?: boolean | null }"
        );
        assert_eq!(
            response_type(&function.sig.output, &mut graph, &context).unwrap(),
            "void"
        );
    }

    #[test]
    fn argument_case_respects_tauri_command_attribute() {
        let source = r#"#[tauri::command(rename_all = "snake_case")] fn change_item(item_id: String) -> Result<(), String> { todo!() }"#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let casing = argument_case(&function.attrs).unwrap();
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert_eq!(casing, ArgumentCase::Snake);
        assert_eq!(
            request_type(&function.sig.inputs, casing, &mut graph, &context).unwrap(),
            "{ item_id: string }"
        );
    }

    #[test]
    fn generator_rejects_unmapped_dynamic_and_non_string_result_types() {
        let source = r#"#[tauri::command] fn command(id: String) -> Result<Vec<u8>, anyhow::Error> { todo!() }"#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert!(
            response_type(&function.sig.output, &mut graph, &context)
                .unwrap_err()
                .contains("must be String")
        );
        assert_eq!(
            request_type(
                &function.sig.inputs,
                ArgumentCase::Camel,
                &mut graph,
                &context
            )
            .unwrap(),
            "{ id: string }"
        );
    }

    #[test]
    fn unknown_request_dto_fails_closed() {
        let source = r#"#[tauri::command] fn command(payload: UnknownDto) -> Result<(), String> { todo!() }"#;
        let file = syn::parse_file(source).unwrap();
        let Item::Fn(function) = &file.items[0] else {
            panic!()
        };
        let mut graph = RustTypeGraph::default();
        let context = RustTypeGraph::test_context();
        assert!(
            request_type(
                &function.sig.inputs,
                ArgumentCase::Camel,
                &mut graph,
                &context
            )
            .unwrap_err()
            .contains("unsupported/unmapped Rust DTO")
        );
    }
}
