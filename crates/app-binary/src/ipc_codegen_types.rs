//! Rust/Serde source exporter used by `generate_ipc_contracts`.
//!
//! It reads the actual serializable DTO declarations and Tauri handler
//! signatures. The generated TypeScript file is the only renderer-side shape
//! source; unsupported Rust wire types fail the generation command.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::{Fields, Item, Type};

#[derive(Clone)]
pub(crate) struct TypeContext {
    crate_name: String,
    module: Vec<String>,
    imports: HashMap<String, Vec<String>>,
}

#[derive(Clone)]
struct FieldDef {
    name: String,
    ty: Type,
    response_optional: bool,
    input_optional: bool,
    skip_serializing: bool,
    skip_deserializing: bool,
}

#[derive(Clone)]
struct VariantDef {
    source_name: String,
    name: String,
    fields: Vec<FieldDef>,
    payload: Option<Type>,
    unit: bool,
    skip_serializing: bool,
    skip_deserializing: bool,
}

#[derive(Clone)]
enum TypeKind {
    Struct(Vec<FieldDef>),
    Enum(Vec<VariantDef>),
    Alias(Type),
    Unsupported(String),
}

#[derive(Clone, Debug, Default)]
struct SerdeOptions {
    rename: Option<String>,
    rename_all: Option<String>,
    rename_all_fields: Option<String>,
    tag: Option<String>,
    content: Option<String>,
    untagged: bool,
    transparent: bool,
    default: bool,
    skip_serializing: bool,
    skip_deserializing: bool,
    skip_serializing_if: Option<String>,
    flatten: bool,
}

#[derive(Clone)]
struct TypeDef {
    key: String,
    name: String,
    context: TypeContext,
    kind: TypeKind,
    serde: SerdeOptions,
    serializable: bool,
    deserializable: bool,
    manual_string_enum: bool,
    manual_variant_names: HashMap<String, String>,
}

#[derive(Default)]
pub(crate) struct RustTypeGraph {
    definitions: BTreeMap<String, TypeDef>,
    by_name: HashMap<String, Vec<String>>,
    emitted: BTreeMap<String, String>,
    emitted_names: HashMap<String, String>,
}

#[derive(Clone, Copy)]
pub(crate) enum TypeUse {
    Request,
    Response,
}

#[derive(Debug)]
pub(crate) struct MappedType {
    pub ts: String,
    pub optional: bool,
}

impl RustTypeGraph {
    pub(crate) fn load(root: &Path) -> Result<Self, String> {
        let crates = root.join("crates");
        let mut source_files = Vec::new();
        for entry in fs::read_dir(&crates).map_err(|error| format!("read crates/: {error}"))? {
            let entry = entry.map_err(|error| format!("list crates/: {error}"))?;
            let src = entry.path().join("src");
            if src.is_dir() {
                collect_rs_files(&src, &mut source_files)?;
            }
        }
        source_files.sort();

        let mut graph = Self::default();
        for file_path in source_files {
            let source = fs::read_to_string(&file_path)
                .map_err(|error| format!("read {}: {error}", file_path.display()))?;
            let parsed = syn::parse_file(&source)
                .map_err(|error| format!("parse {}: {error}", file_path.display()))?;
            let context = context_for_file(root, &file_path, &parsed.items)?;
            graph.collect_items(&parsed.items, &context)?;
        }
        Ok(graph)
    }

    pub(crate) fn context_for_command(
        root: &Path,
        file_path: &Path,
        items: &[Item],
    ) -> Result<TypeContext, String> {
        context_for_file(root, file_path, items)
    }

    #[cfg(test)]
    pub(crate) fn test_context() -> TypeContext {
        TypeContext {
            crate_name: "test_crate".into(),
            module: Vec::new(),
            imports: HashMap::new(),
        }
    }

    pub(crate) fn map_type(
        &mut self,
        ty: &Type,
        usage: TypeUse,
        context: &TypeContext,
    ) -> Result<MappedType, String> {
        match ty {
            Type::Reference(value) => self.map_type(&value.elem, usage, context),
            Type::Paren(value) => self.map_type(&value.elem, usage, context),
            Type::Group(value) => self.map_type(&value.elem, usage, context),
            Type::Tuple(value) if value.elems.is_empty() => Ok(MappedType {
                ts: "void".into(),
                optional: false,
            }),
            Type::Tuple(value) => {
                let mut fields = Vec::new();
                for element in &value.elems {
                    fields.push(self.map_type(element, usage, context)?.ts);
                }
                Ok(MappedType {
                    ts: format!("[{}]", fields.join(", ")),
                    optional: false,
                })
            }
            Type::Array(value) => Ok(MappedType {
                ts: format!("{}[]", self.map_type(&value.elem, usage, context)?.ts),
                optional: false,
            }),
            Type::Path(value) => {
                let Some(segment) = value.path.segments.last() else {
                    return Err("empty Rust IPC type path".into());
                };
                let name = segment.ident.to_string();
                let arguments = match &segment.arguments {
                    syn::PathArguments::AngleBracketed(arguments) => Some(&arguments.args),
                    syn::PathArguments::None => None,
                    other => {
                        return Err(format!(
                            "unsupported generic IPC type {}",
                            other.to_token_stream()
                        ));
                    }
                };
                match name.as_str() {
                    "String" | "str" | "PathBuf" | "Uuid" => Ok(MappedType {
                        ts: "string".into(),
                        optional: false,
                    }),
                    "bool" => Ok(MappedType {
                        ts: "boolean".into(),
                        optional: false,
                    }),
                    "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32"
                    | "u64" | "u128" | "usize" | "f32" | "f64" => Ok(MappedType {
                        ts: "number".into(),
                        optional: false,
                    }),
                    "Value" if self.is_json_value(&value.path, context) => Ok(MappedType {
                        ts: "unknown".into(),
                        optional: false,
                    }),
                    "Option" => {
                        let inner = one_type_argument(arguments, &name)?;
                        let mapped = self.map_type(inner, usage, context)?;
                        let ts = if mapped.ts == "void" {
                            "null".into()
                        } else {
                            format!("{} | null", mapped.ts)
                        };
                        Ok(MappedType {
                            ts,
                            optional: matches!(usage, TypeUse::Request),
                        })
                    }
                    "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => {
                        let inner = one_type_argument(arguments, &name)?;
                        Ok(MappedType {
                            ts: format!("{}[]", self.map_type(inner, usage, context)?.ts),
                            optional: false,
                        })
                    }
                    "HashMap" | "BTreeMap" => {
                        let args =
                            arguments.ok_or_else(|| format!("{name} missing generic arguments"))?;
                        let types = args
                            .iter()
                            .filter_map(|argument| match argument {
                                syn::GenericArgument::Type(ty) => Some(ty),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if types.len() != 2 {
                            return Err(format!("{name} requires key and value types"));
                        }
                        let key = self.map_type(types[0], usage, context)?.ts;
                        if key != "string" {
                            return Err(format!("IPC map keys must be strings, found {key}"));
                        }
                        let value = self.map_type(types[1], usage, context)?.ts;
                        Ok(MappedType {
                            ts: format!("Record<string, {value}>"),
                            optional: false,
                        })
                    }
                    "Arc" | "Box" | "Rc" | "Cow" => {
                        self.map_type(one_type_argument(arguments, &name)?, usage, context)
                    }
                    "Result" => Err("nested Result is not a stable IPC DTO".into()),
                    _ => {
                        let key = self.resolve_type(&value.path, context)?;
                        let ts = self.emit_definition(&key, usage)?;
                        Ok(MappedType {
                            ts,
                            optional: false,
                        })
                    }
                }
            }
            other => Err(format!(
                "unsupported Rust IPC type: {}",
                other.to_token_stream()
            )),
        }
    }

    pub(crate) fn declarations(&self) -> String {
        let mut output = String::new();
        for declaration in self.emitted.values() {
            output.push_str(declaration);
            output.push('\n');
        }
        output
    }

    fn collect_items(&mut self, items: &[Item], context: &TypeContext) -> Result<(), String> {
        for item in items {
            match item {
                Item::Struct(item) => {
                    let serializable = has_derive(&item.attrs, "Serialize");
                    let deserializable = has_derive(&item.attrs, "Deserialize");
                    match fields_from_struct(&item.fields, &item.attrs) {
                        Ok(fields) => self.insert_definition(
                            context,
                            &item.ident.to_string(),
                            fields,
                            &item.attrs,
                            serializable,
                            deserializable,
                        )?,
                        Err(error) => self.insert_unsupported(
                            context,
                            &item.ident.to_string(),
                            error,
                            &item.attrs,
                            serializable,
                            deserializable,
                        )?,
                    }
                }
                Item::Enum(item) => {
                    let variants = (|| {
                        let mut variants = Vec::new();
                        let rename_all = container_case(&item.attrs)?;
                        for variant in &item.variants {
                            let options = serde_options(&variant.attrs)?;
                            let name = match options.rename {
                                Some(name) => name,
                                None => {
                                    apply_case(&variant.ident.to_string(), rename_all.as_deref())?
                                }
                            };
                            let (fields, payload) =
                                variant_fields(&variant.fields, &variant.attrs)?;
                            variants.push(VariantDef {
                                source_name: variant.ident.to_string(),
                                name,
                                unit: matches!(variant.fields, Fields::Unit),
                                fields,
                                payload,
                                skip_serializing: options.skip_serializing,
                                skip_deserializing: options.skip_deserializing,
                            });
                        }
                        Ok::<_, String>(variants)
                    })();
                    let serializable = has_derive(&item.attrs, "Serialize");
                    let deserializable = has_derive(&item.attrs, "Deserialize");
                    match variants {
                        Ok(variants) => self.insert_enum(
                            context,
                            &item.ident.to_string(),
                            variants,
                            &item.attrs,
                            serializable,
                            deserializable,
                        )?,
                        Err(error) => self.insert_unsupported(
                            context,
                            &item.ident.to_string(),
                            error,
                            &item.attrs,
                            serializable,
                            deserializable,
                        )?,
                    }
                }
                Item::Type(item) => {
                    let serializable = has_derive(&item.attrs, "Serialize");
                    let deserializable = has_derive(&item.attrs, "Deserialize");
                    // Platform implementation aliases and other internal type
                    // aliases are not IPC DTOs. Ignoring them also avoids
                    // treating mutually exclusive cfg declarations as
                    // duplicate wire types.
                    if serializable || deserializable {
                        self.insert_alias(
                            context,
                            &item.ident.to_string(),
                            (*item.ty).clone(),
                            &item.attrs,
                            serializable,
                            deserializable,
                        )?;
                    }
                }
                Item::Macro(item)
                    if item
                        .mac
                        .path
                        .segments
                        .last()
                        .is_some_and(|part| part.ident == "settings_pair") =>
                {
                    let (name, fields) = parse_settings_pair(&item.mac.tokens)?;
                    // The macro emits both aggregate structs with
                    // `#[serde(default)]`; the input contract for Settings
                    // must reflect that actual generated Rust declaration.
                    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(default)])];
                    self.insert_definition(context, &name, fields, &attrs, true, true)?;
                }
                Item::Mod(item) if item.content.is_some() => {
                    let mut nested = context.clone();
                    nested.module.push(item.ident.to_string());
                    self.collect_items(&item.content.as_ref().unwrap().1, &nested)?;
                }
                Item::Impl(item) => self.collect_impl(item, context)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn collect_impl(&mut self, item: &syn::ItemImpl, context: &TypeContext) -> Result<(), String> {
        let Type::Path(self_path) = item.self_ty.as_ref() else {
            return Ok(());
        };
        let type_name = self_path
            .path
            .segments
            .last()
            .ok_or_else(|| "impl block has an empty self type".to_string())?
            .ident
            .to_string();
        let key = type_key(context, &type_name);
        if let Some((_, trait_path, _)) = &item.trait_ {
            if trait_path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Serialize")
                && let Some(definition) = self.definitions.get_mut(&key)
            {
                let source = item.to_token_stream().to_string();
                definition.serializable = true;
                if source.contains("serialize_str") && source.contains("as_str") {
                    definition.manual_string_enum = true;
                } else {
                    definition.kind = TypeKind::Unsupported(
                        "manual Serialize implementation needs an explicit mapping".into(),
                    );
                }
            }
        } else {
            for member in &item.items {
                let syn::ImplItem::Fn(function) = member else {
                    continue;
                };
                if function.sig.ident != "as_str" {
                    continue;
                }
                let values = string_match_arms(&function.block);
                if let Some(definition) = self.definitions.get_mut(&key) {
                    definition.manual_variant_names.extend(values);
                }
            }
        }
        Ok(())
    }

    fn insert_definition(
        &mut self,
        context: &TypeContext,
        name: &str,
        fields: Vec<FieldDef>,
        attrs: &[syn::Attribute],
        serializable: bool,
        deserializable: bool,
    ) -> Result<(), String> {
        self.insert(TypeDef {
            key: type_key(context, name),
            name: name.to_string(),
            context: context.clone(),
            kind: TypeKind::Struct(fields),
            serde: serde_options(attrs)?,
            serializable,
            deserializable,
            manual_string_enum: false,
            manual_variant_names: HashMap::new(),
        })
    }

    fn insert_unsupported(
        &mut self,
        context: &TypeContext,
        name: &str,
        reason: String,
        attrs: &[syn::Attribute],
        serializable: bool,
        deserializable: bool,
    ) -> Result<(), String> {
        self.insert(TypeDef {
            key: type_key(context, name),
            name: name.to_string(),
            context: context.clone(),
            kind: TypeKind::Unsupported(reason),
            serde: serde_options(attrs)?,
            serializable,
            deserializable,
            manual_string_enum: false,
            manual_variant_names: HashMap::new(),
        })
    }

    fn insert_enum(
        &mut self,
        context: &TypeContext,
        name: &str,
        variants: Vec<VariantDef>,
        attrs: &[syn::Attribute],
        serializable: bool,
        deserializable: bool,
    ) -> Result<(), String> {
        self.insert(TypeDef {
            key: type_key(context, name),
            name: name.to_string(),
            context: context.clone(),
            kind: TypeKind::Enum(variants),
            serde: serde_options(attrs)?,
            serializable,
            deserializable,
            manual_string_enum: false,
            manual_variant_names: HashMap::new(),
        })
    }

    fn insert_alias(
        &mut self,
        context: &TypeContext,
        name: &str,
        ty: Type,
        attrs: &[syn::Attribute],
        serializable: bool,
        deserializable: bool,
    ) -> Result<(), String> {
        self.insert(TypeDef {
            key: type_key(context, name),
            name: name.to_string(),
            context: context.clone(),
            kind: TypeKind::Alias(ty),
            serde: serde_options(attrs)?,
            serializable,
            deserializable,
            manual_string_enum: false,
            manual_variant_names: HashMap::new(),
        })
    }

    fn insert(&mut self, definition: TypeDef) -> Result<(), String> {
        if self.definitions.contains_key(&definition.key) {
            return Err(format!("duplicate Rust DTO source {}", definition.key));
        }
        self.by_name
            .entry(definition.name.clone())
            .or_default()
            .push(definition.key.clone());
        self.definitions.insert(definition.key.clone(), definition);
        Ok(())
    }

    fn emit_definition(&mut self, key: &str, usage: TypeUse) -> Result<String, String> {
        if let Some(definition) = self.definitions.get(key) {
            let derives_required_trait = match usage {
                TypeUse::Request => definition.deserializable,
                TypeUse::Response => definition.serializable,
            };
            if !derives_required_trait {
                return Err(format!(
                    "Rust IPC DTO {} does not derive {}",
                    definition.key,
                    match usage {
                        TypeUse::Request => "Deserialize",
                        TypeUse::Response => "Serialize",
                    }
                ));
            }
            let name = emitted_name(&definition.name, usage);
            let emitted_key = emitted_key(key, usage);
            if let Some(owner) = self.emitted_names.get(&name) {
                if owner != &emitted_key {
                    return Err(format!(
                        "TypeScript DTO name `{name}` is ambiguous: {owner} and {key}"
                    ));
                }
            } else {
                self.emitted_names.insert(name.clone(), emitted_key.clone());
            }
            if self.emitted.contains_key(&emitted_key) {
                return Ok(name);
            }
            self.emitted.insert(emitted_key.clone(), String::new());
            let declaration = self.render_definition(key, usage)?;
            self.emitted.insert(emitted_key, declaration);
            Ok(name)
        } else {
            Err(format!("no Rust DTO declaration for {key}"))
        }
    }

    fn render_definition(&mut self, key: &str, usage: TypeUse) -> Result<String, String> {
        let definition = self.definitions.get(key).cloned().expect("known key");
        let name = emitted_name(&definition.name, usage);
        if definition.serde.flatten {
            return Err(format!(
                "{} uses unsupported serde(flatten)",
                definition.key
            ));
        }
        match definition.kind.clone() {
            TypeKind::Unsupported(reason) => Err(format!(
                "Rust IPC DTO {} needs an explicit mapping: {reason}",
                definition.key
            )),
            TypeKind::Alias(ty) => {
                let mapped = self.map_type(&ty, usage, &definition.context)?;
                Ok(format!("export type {} = {};", name, mapped.ts))
            }
            TypeKind::Struct(fields) => {
                if definition.serde.transparent {
                    if fields.len() != 1 {
                        return Err(format!(
                            "{} transparent DTO must have exactly one field",
                            definition.key
                        ));
                    }
                    let mapped = self.map_type(&fields[0].ty, usage, &definition.context)?;
                    return Ok(format!("export type {} = {};", name, mapped.ts));
                }
                let mut rendered = Vec::new();
                for field in fields {
                    if matches!(usage, TypeUse::Response) && field.skip_serializing {
                        continue;
                    }
                    if matches!(usage, TypeUse::Request) && field.skip_deserializing {
                        continue;
                    }
                    let (mapped, optional) = match usage {
                        TypeUse::Response if field.response_optional => {
                            if let Some(inner) = option_inner(&field.ty)? {
                                (self.map_type(inner, usage, &definition.context)?, true)
                            } else {
                                (self.map_type(&field.ty, usage, &definition.context)?, true)
                            }
                        }
                        TypeUse::Response => {
                            (self.map_type(&field.ty, usage, &definition.context)?, false)
                        }
                        TypeUse::Request => {
                            let mapped = self.map_type(&field.ty, usage, &definition.context)?;
                            let optional =
                                definition.serde.default || field.input_optional || mapped.optional;
                            (mapped, optional)
                        }
                    };
                    let suffix = if optional { "?" } else { "" };
                    rendered.push(format!("{}{}: {}", field.name, suffix, mapped.ts));
                }
                Ok(format!(
                    "export interface {} {{ {} }}",
                    name,
                    rendered.join("; ")
                ))
            }
            TypeKind::Enum(variants) => self.render_enum(&definition, variants, usage, &name),
        }
    }

    fn render_enum(
        &mut self,
        definition: &TypeDef,
        variants: Vec<VariantDef>,
        usage: TypeUse,
        emitted_type_name: &str,
    ) -> Result<String, String> {
        let is_plain_string_enum = definition.manual_string_enum
            || (!definition.serde.untagged
                && definition.serde.tag.is_none()
                && definition.serde.content.is_none()
                && variants.iter().all(|variant| variant.unit));
        if is_plain_string_enum {
            let mut values = Vec::new();
            for variant in variants {
                if matches!(usage, TypeUse::Response) && variant.skip_serializing {
                    continue;
                }
                if matches!(usage, TypeUse::Request) && variant.skip_deserializing {
                    continue;
                }
                let serialized = if definition.manual_string_enum {
                    definition
                        .manual_variant_names
                        .get(&variant.source_name)
                        .ok_or_else(|| {
                            format!(
                                "manual Serialize for {} has no as_str mapping for {}",
                                definition.name, variant.source_name
                            )
                        })?
                        .clone()
                } else {
                    variant.name.clone()
                };
                values.push(quote_ts_string(&serialized));
            }
            let values_name = apply_case(
                &format!("{emitted_type_name}_values"),
                Some("SCREAMING_SNAKE_CASE"),
            )?;
            return Ok(format!(
                "export const {values_name} = [{}] as const;\nexport type {emitted_type_name} = (typeof {values_name})[number];",
                values.join(", ")
            ));
        }
        if definition.serde.untagged {
            let mut choices = Vec::new();
            for variant in variants {
                if matches!(usage, TypeUse::Response) && variant.skip_serializing {
                    continue;
                }
                if matches!(usage, TypeUse::Request) && variant.skip_deserializing {
                    continue;
                }
                choices.push(self.render_variant_payload(&variant, &definition.context, usage)?);
            }
            return Ok(format!(
                "export type {} = {};",
                emitted_type_name,
                choices.join(" | ")
            ));
        }
        let rename_all_fields = definition.serde.rename_all_fields.as_deref();
        let mut choices = Vec::new();
        for variant in variants {
            if matches!(usage, TypeUse::Response) && variant.skip_serializing {
                continue;
            }
            if matches!(usage, TypeUse::Request) && variant.skip_deserializing {
                continue;
            }
            let variant_name = quote_ts_string(&variant.name);
            if variant.unit {
                if let Some(tag) = &definition.serde.tag {
                    choices.push(format!("{{ {}: {} }}", tag, variant_name));
                } else if definition.serde.content.is_some() {
                    let tag = definition.serde.tag.as_deref().unwrap_or("type");
                    let content = definition.serde.content.as_deref().unwrap();
                    choices.push(format!(
                        "{{ {}: {}; {}?: never }}",
                        tag, variant_name, content
                    ));
                } else {
                    choices.push(variant_name);
                }
                continue;
            }
            let payload = self.render_variant_payload_with_case(
                &variant,
                &definition.context,
                rename_all_fields,
                usage,
            )?;
            if let Some(tag) = &definition.serde.tag {
                if let Some(content) = &definition.serde.content {
                    choices.push(format!(
                        "{{ {}: {}; {}: {} }}",
                        tag, variant_name, content, payload
                    ));
                } else if payload.starts_with("{") {
                    choices.push(payload.replacen(
                        '{',
                        &format!("{{ {}: {};", tag, variant_name),
                        1,
                    ));
                } else {
                    return Err(format!(
                        "internally tagged enum {} has non-object variant",
                        definition.name
                    ));
                }
            } else if let Some(content) = &definition.serde.content {
                let tag = definition.serde.tag.as_deref().unwrap_or("type");
                choices.push(format!(
                    "{{ {}: {}; {}: {} }}",
                    tag, variant_name, content, payload
                ));
            } else {
                choices.push(format!("{{ {}: {} }}", variant_name, payload));
            }
        }
        Ok(format!(
            "export type {} = {};",
            emitted_type_name,
            choices.join(" | ")
        ))
    }

    fn render_variant_payload(
        &mut self,
        variant: &VariantDef,
        context: &TypeContext,
        usage: TypeUse,
    ) -> Result<String, String> {
        self.render_variant_payload_with_case(variant, context, None, usage)
    }

    fn render_variant_payload_with_case(
        &mut self,
        variant: &VariantDef,
        context: &TypeContext,
        case: Option<&str>,
        usage: TypeUse,
    ) -> Result<String, String> {
        if let Some(payload) = &variant.payload {
            return Ok(self.map_type(payload, usage, context)?.ts);
        }
        let mut fields = Vec::new();
        for field in &variant.fields {
            if matches!(usage, TypeUse::Response) && field.skip_serializing {
                continue;
            }
            if matches!(usage, TypeUse::Request) && field.skip_deserializing {
                continue;
            }
            let mapped = self.map_type(&field.ty, usage, context)?;
            let name = apply_case(&field.name, case)?;
            let optional = match usage {
                TypeUse::Response if field.response_optional => "?",
                TypeUse::Request if field.input_optional || mapped.optional => "?",
                _ => "",
            };
            fields.push(format!("{name}{optional}: {}", mapped.ts));
        }
        if variant.unit {
            Ok("{}".into())
        } else if fields.is_empty() {
            Err("tuple enum variants require an explicit serde DTO mapping".into())
        } else {
            Ok(format!("{{ {} }}", fields.join("; ")))
        }
    }

    fn resolve_type(&self, path: &syn::Path, context: &TypeContext) -> Result<String, String> {
        let full = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let last = full.last().ok_or_else(|| "empty DTO path".to_string())?;
        if full.len() == 1 {
            let local = type_key(context, last);
            if self.definitions.contains_key(&local) {
                return Ok(local);
            }
            if let Some(import) = context.imports.get(last) {
                let import = normalize_module_path(import, context);
                if let Some(key) = self.resolve_candidate(&import) {
                    return Ok(key);
                }
            }
        } else {
            let mut expanded = full.clone();
            if expanded.first().is_some_and(|part| part == "crate") {
                expanded[0] = context.crate_name.clone();
            } else if expanded.first().is_some_and(|part| part == "self") {
                expanded.splice(0..1, context.module.clone());
            } else if expanded.first().is_some_and(|part| part == "super") {
                let mut module = context.module.clone();
                while expanded.first().is_some_and(|part| part == "super") {
                    expanded.remove(0);
                    module.pop();
                }
                expanded.splice(0..0, module);
            }
            let candidate = expanded.join("::");
            if self.definitions.contains_key(&candidate) {
                return Ok(candidate);
            }
            if let Some(import) = context.imports.get(last) {
                let import = normalize_module_path(import, context);
                if let Some(key) = self.resolve_candidate(&import) {
                    return Ok(key);
                }
            }
        }
        let candidates = self.by_name.get(last).cloned().unwrap_or_default();
        let local_candidates = candidates
            .iter()
            .filter(|candidate| candidate.starts_with(&format!("{}::", context.crate_name)))
            .cloned()
            .collect::<Vec<_>>();
        match local_candidates.as_slice() {
            [candidate] => return Ok(candidate.clone()),
            [] => {}
            _ => {
                return Err(format!(
                    "ambiguous Rust DTO type `{last}` in crate `{}` from `{}`; candidates: {}",
                    context.crate_name,
                    path.to_token_stream(),
                    local_candidates.join(", ")
                ));
            }
        }
        match candidates.as_slice() {
            [candidate] => Ok(candidate.clone()),
            [] => Err(format!(
                "unsupported/unmapped Rust DTO type `{}`",
                path.to_token_stream()
            )),
            _ => Err(format!(
                "ambiguous Rust DTO type `{last}` from `{}`; candidates: {}",
                path.to_token_stream(),
                candidates.join(", ")
            )),
        }
    }

    fn resolve_candidate(&self, import: &[String]) -> Option<String> {
        let key = import.join("::");
        if self.definitions.contains_key(&key) {
            return Some(key);
        }
        let last = import.last()?;
        let candidates = self.by_name.get(last)?;
        (candidates.len() == 1).then(|| candidates[0].clone())
    }

    fn is_json_value(&self, path: &syn::Path, context: &TypeContext) -> bool {
        let segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if segments.len() >= 2 && segments[segments.len() - 2..] == ["serde_json", "Value"] {
            return true;
        }
        segments.len() == 1
            && context
                .imports
                .get("Value")
                .is_some_and(|import| import == &["serde_json".to_string(), "Value".to_string()])
    }
}

fn collect_rs_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|error| format!("read {}: {error}", root.display()))? {
        let entry = entry.map_err(|error| format!("list {}: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
    Ok(())
}

fn context_for_file(root: &Path, path: &Path, items: &[Item]) -> Result<TypeContext, String> {
    let crate_dir = path
        .strip_prefix(root.join("crates"))
        .map_err(|error| format!("{} is outside crates/: {error}", path.display()))?
        .components()
        .next()
        .ok_or_else(|| format!("could not infer crate for {}", path.display()))?
        .as_os_str()
        .to_string_lossy()
        .to_string();
    let manifest_path = root.join("crates").join(&crate_dir).join("Cargo.toml");
    let crate_name = fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|manifest| {
            let mut section = String::new();
            let mut package_name = None;
            let mut library_name = None;
            for line in manifest.lines().map(str::trim) {
                if line.starts_with('[') && line.ends_with(']') {
                    section = line.to_string();
                } else if let Some(value) = line
                    .strip_prefix("name = \"")
                    .and_then(|value| value.strip_suffix('"'))
                {
                    if section == "[package]" {
                        package_name = Some(value.to_string());
                    } else if section == "[lib]" {
                        library_name = Some(value.to_string());
                    }
                }
            }
            library_name.or(package_name)
        })
        .unwrap_or_else(|| crate_dir.clone())
        .replace('-', "_");
    let src = root.join("crates").join(&crate_dir).join("src");
    let relative = path.strip_prefix(&src).unwrap_or(path);
    let mut module = relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if module
        .last()
        .is_some_and(|part| part == "lib" || part == "main" || part == "mod")
    {
        module.pop();
    }
    let mut imports = HashMap::new();
    for item in items {
        if let Item::Use(item) = item {
            flatten_use_tree(&[], &item.tree, &mut imports);
        }
    }
    Ok(TypeContext {
        crate_name,
        module,
        imports,
    })
}

fn flatten_use_tree(
    prefix: &[String],
    tree: &syn::UseTree,
    imports: &mut HashMap<String, Vec<String>>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            let mut next = prefix.to_vec();
            next.push(path.ident.to_string());
            flatten_use_tree(&next, &path.tree, imports);
        }
        syn::UseTree::Name(name) => {
            let mut path = prefix.to_vec();
            let ident = name.ident.to_string();
            if ident != "self" {
                path.push(ident.clone());
                imports.insert(ident, path);
            }
        }
        syn::UseTree::Rename(rename) => {
            let mut path = prefix.to_vec();
            path.push(rename.ident.to_string());
            imports.insert(rename.rename.to_string(), path);
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_tree(prefix, item, imports);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn fields_from_struct(fields: &Fields, attrs: &[syn::Attribute]) -> Result<Vec<FieldDef>, String> {
    let options = serde_options(attrs)?;
    match fields {
        Fields::Named(named) => {
            let mut output = Vec::new();
            for field in &named.named {
                let field_options = serde_options(&field.attrs)?;
                if field_options.flatten {
                    return Err("serde(flatten) fields require an explicit DTO mapping".into());
                }
                let field_name = field.ident.as_ref().expect("named field").to_string();
                let name = match field_options.rename {
                    Some(name) => name,
                    None => apply_case(&field_name, options.rename_all.as_deref())?,
                };
                output.push(FieldDef {
                    name,
                    ty: field.ty.clone(),
                    response_optional: is_optional_wire_field(
                        field_options.skip_serializing_if.as_deref(),
                        &field.ty,
                    )?,
                    input_optional: options.default
                        || field_options.default
                        || is_option_type(&field.ty),
                    skip_serializing: field_options.skip_serializing,
                    skip_deserializing: field_options.skip_deserializing,
                });
            }
            Ok(output)
        }
        Fields::Unit => Ok(Vec::new()),
        Fields::Unnamed(unnamed) if options.transparent && unnamed.unnamed.len() == 1 => {
            Ok(vec![FieldDef {
                name: "value".into(),
                ty: unnamed.unnamed[0].ty.clone(),
                response_optional: false,
                input_optional: options.default || is_option_type(&unnamed.unnamed[0].ty),
                skip_serializing: false,
                skip_deserializing: false,
            }])
        }
        Fields::Unnamed(_) => {
            Err("tuple DTO fields require #[serde(transparent)] or named fields".into())
        }
    }
}

fn variant_fields(
    fields: &Fields,
    attrs: &[syn::Attribute],
) -> Result<(Vec<FieldDef>, Option<Type>), String> {
    match fields {
        Fields::Unnamed(unnamed) => {
            let mut types = Vec::new();
            for field in &unnamed.unnamed {
                let options = serde_options(&field.attrs)?;
                if options.skip_serializing
                    || options.skip_deserializing
                    || options.default
                    || options.flatten
                    || options.skip_serializing_if.is_some()
                {
                    return Err("tuple enum field has unsupported serde field attributes".into());
                }
                types.push(field.ty.clone());
            }
            if types.len() == 1 {
                Ok((Vec::new(), Some(types.pop().unwrap())))
            } else {
                let payload: Type = syn::parse_quote!((#(#types),*));
                Ok((Vec::new(), Some(payload)))
            }
        }
        Fields::Named(_) => Ok((fields_from_struct(fields, attrs)?, None)),
        Fields::Unit => Ok((Vec::new(), None)),
    }
}

fn has_derive(attrs: &[syn::Attribute], target: &str) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .any(|attr| {
            attr.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|paths| {
                paths.iter().any(|path| {
                    path.segments
                        .last()
                        .is_some_and(|part| part.ident == target)
                })
            })
        })
}

fn serde_options(attrs: &[syn::Attribute]) -> Result<SerdeOptions, String> {
    let mut options = SerdeOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                options.rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("rename_all") {
                options.rename_all = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("rename_all_fields") {
                options.rename_all_fields = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("tag") {
                options.tag = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("content") {
                options.content = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("untagged") {
                options.untagged = true;
            } else if meta.path.is_ident("transparent") {
                options.transparent = true;
            } else if meta.path.is_ident("skip") {
                options.skip_serializing = true;
                options.skip_deserializing = true;
            } else if meta.path.is_ident("skip_serializing") {
                options.skip_serializing = true;
            } else if meta.path.is_ident("skip_deserializing") {
                options.skip_deserializing = true;
            } else if meta.path.is_ident("skip_serializing_if") {
                options.skip_serializing_if = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("flatten") {
                options.flatten = true;
            } else if meta.path.is_ident("default") {
                options.default = true;
                if meta.input.peek(syn::Token![=]) {
                    let _ = meta.value()?.parse::<syn::Expr>()?;
                }
            } else if meta.path.is_ident("alias") {
                if meta.input.peek(syn::Token![=]) {
                    let _ = meta.value()?.parse::<syn::Expr>()?;
                }
            } else if meta.path.is_ident("deny_unknown_fields")
                || meta.path.is_ident("bound")
                || meta.path.is_ident("borrow")
                || meta.path.is_ident("other")
            {
                // These attributes do not change the emitted JSON shape.
                if meta.input.peek(syn::Token![=]) {
                    let _ = meta.value()?.parse::<syn::Expr>()?;
                }
            } else if meta.path.is_ident("serialize_with")
                || meta.path.is_ident("deserialize_with")
                || meta.path.is_ident("with")
                || meta.path.is_ident("remote")
                || meta.path.is_ident("from")
                || meta.path.is_ident("try_from")
                || meta.path.is_ident("into")
            {
                return Err(
                    meta.error("custom Serde wire behavior needs an explicit Rust IPC mapping")
                );
            } else {
                return Err(meta.error("unsupported serde metadata in Rust IPC DTO"));
            }
            Ok(())
        })
        .map_err(|error| format!("unsupported serde attributes: {error}"))?;
    }
    Ok(options)
}

fn container_case(attrs: &[syn::Attribute]) -> Result<Option<String>, String> {
    Ok(serde_options(attrs)?.rename_all)
}

fn is_optional_wire_field(predicate: Option<&str>, ty: &Type) -> Result<bool, String> {
    let Some(predicate) = predicate else {
        return Ok(false);
    };
    let Type::Path(path) = ty else {
        return Err(format!(
            "skip_serializing_if predicate `{predicate}` needs a path field type"
        ));
    };
    let type_name = path
        .path
        .segments
        .last()
        .map(|part| part.ident.to_string())
        .unwrap_or_default();
    match (predicate, type_name.as_str()) {
        ("Option::is_none", "Option") => Ok(true),
        ("Vec::is_empty", "Vec") => Ok(true),
        _ => Err(format!(
            "unsupported skip_serializing_if `{predicate}` on {type_name}; add an explicit serializer mapping"
        )),
    }
}

fn is_option_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "Option"))
}

fn option_inner(ty: &Type) -> Result<Option<&Type>, String> {
    let Type::Path(path) = ty else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != "Option" {
        return Ok(None);
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err("Option is missing its inner type".into());
    };
    one_type_argument(Some(&arguments.args), "Option").map(Some)
}

fn emitted_name(name: &str, usage: TypeUse) -> String {
    match usage {
        TypeUse::Request => format!("{name}Input"),
        TypeUse::Response => name.to_string(),
    }
}

fn emitted_key(key: &str, usage: TypeUse) -> String {
    match usage {
        TypeUse::Request => format!("{key}#input"),
        TypeUse::Response => format!("{key}#response"),
    }
}

fn apply_case(value: &str, case: Option<&str>) -> Result<String, String> {
    let output = match case {
        Some("camelCase") => {
            let words = case_words(value);
            let mut output = words.first().cloned().unwrap_or_default();
            for word in words.iter().skip(1) {
                let mut chars = word.chars();
                if let Some(first) = chars.next() {
                    output.push(first.to_ascii_uppercase());
                    output.extend(chars);
                }
            }
            output
        }
        Some("PascalCase") => case_words(value)
            .iter()
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect(),
        Some("kebab-case") => case_words(value).join("-"),
        Some("SCREAMING-KEBAB-CASE") => case_words(value).join("-").to_ascii_uppercase(),
        Some("UPPERCASE") => case_words(value).join("").to_ascii_uppercase(),
        Some("lowercase") => case_words(value).join("").to_ascii_lowercase(),
        Some("SCREAMING_SNAKE_CASE") => case_words(value).join("_").to_ascii_uppercase(),
        Some("snake_case") => case_words(value).join("_"),
        None => value.to_string(),
        Some(other) => return Err(format!("unsupported serde rename rule {other:?}")),
    };
    Ok(output)
}

fn case_words(value: &str) -> Vec<String> {
    let chars = value.chars().collect::<Vec<_>>();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch == '_' || ch == '-' || ch == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
            continue;
        }
        let previous = index
            .checked_sub(1)
            .and_then(|previous| chars.get(previous))
            .copied();
        let next = chars.get(index + 1).copied();
        let uppercase_boundary = ch.is_ascii_uppercase()
            && !current.is_empty()
            && (previous.is_some_and(|previous| {
                previous.is_ascii_lowercase() || previous.is_ascii_digit()
            }) || (previous.is_some_and(|previous| previous.is_ascii_uppercase())
                && next.is_some_and(|next| next.is_ascii_lowercase())));
        if uppercase_boundary {
            words.push(std::mem::take(&mut current).to_ascii_lowercase());
        }
        current.push(ch);
    }
    if !current.is_empty() {
        words.push(current.to_ascii_lowercase());
    }
    words
}

fn type_key(context: &TypeContext, name: &str) -> String {
    let mut parts = vec![context.crate_name.clone()];
    parts.extend(context.module.clone());
    parts.push(name.to_string());
    parts.join("::")
}

fn normalize_module_path(path: &[String], context: &TypeContext) -> Vec<String> {
    let mut parts = path.to_vec();
    if parts.first().is_some_and(|part| part == "crate") {
        parts[0] = context.crate_name.clone();
    } else if parts.first().is_some_and(|part| part == "self") {
        parts.splice(0..1, context.module.clone());
    } else if parts.first().is_some_and(|part| part == "super") {
        let mut module = context.module.clone();
        while parts.first().is_some_and(|part| part == "super") {
            parts.remove(0);
            module.pop();
        }
        parts.splice(0..0, module);
    }
    parts
}

fn one_type_argument<'a>(
    arguments: Option<&'a syn::punctuated::Punctuated<syn::GenericArgument, syn::Token![,]>>,
    name: &str,
) -> Result<&'a Type, String> {
    let arguments = arguments.ok_or_else(|| format!("{name} missing generic argument"))?;
    let types = arguments
        .iter()
        .filter_map(|argument| match argument {
            syn::GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != 1 {
        return Err(format!("{name} requires one type argument"));
    }
    Ok(types[0])
}

fn quote_ts_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn string_match_arms(block: &syn::Block) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for statement in &block.stmts {
        let syn::Stmt::Expr(syn::Expr::Match(expression), _) = statement else {
            continue;
        };
        for arm in &expression.arms {
            let syn::Pat::Path(pattern) = &arm.pat else {
                continue;
            };
            let Some(variant) = pattern.path.segments.last() else {
                continue;
            };
            let syn::Expr::Lit(literal) = arm.body.as_ref() else {
                continue;
            };
            let syn::Lit::Str(value) = &literal.lit else {
                continue;
            };
            values.insert(variant.ident.to_string(), value.value());
        }
    }
    values
}

fn parse_settings_pair(
    tokens: &proc_macro2::TokenStream,
) -> Result<(String, Vec<FieldDef>), String> {
    struct Parser {
        name: String,
        fields: Vec<FieldDef>,
    }
    impl syn::parse::Parse for Parser {
        fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
            let name: syn::Ident = input.parse()?;
            input.parse::<syn::Token![;]>()?;
            let mut fields = Vec::new();
            while !input.peek(syn::Token![;]) {
                let attrs = input.call(syn::Attribute::parse_outer)?;
                let ident: syn::Ident = input.parse()?;
                input.parse::<syn::Token![:]>()?;
                let ty: Type = input.parse()?;
                let options =
                    serde_options(&attrs).map_err(|error| syn::Error::new(input.span(), error))?;
                let response_optional =
                    is_optional_wire_field(options.skip_serializing_if.as_deref(), &ty)
                        .map_err(|error| syn::Error::new(input.span(), error))?;
                let input_optional = options.default || is_option_type(&ty);
                fields.push(FieldDef {
                    name: options.rename.unwrap_or_else(|| ident.to_string()),
                    ty,
                    response_optional,
                    input_optional,
                    skip_serializing: options.skip_serializing,
                    skip_deserializing: options.skip_deserializing,
                });
                if input.peek(syn::Token![,]) {
                    input.parse::<syn::Token![,]>()?;
                }
            }
            input.parse::<syn::Token![;]>()?;
            let _: proc_macro2::TokenStream = input.parse()?;
            Ok(Self {
                name: name.to_string(),
                fields,
            })
        }
    }
    let parsed = syn::parse2::<Parser>(tokens.clone())
        .map_err(|error| format!("parse settings_pair!: {error}"))?;
    let _settings_variable = parsed.name;
    Ok(("Settings".into(), parsed.fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_field_names_skip_rules_and_optional_wire_values_are_preserved() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[serde(rename_all = "camelCase")]
            struct Example {
                plain_name: String,
                #[serde(skip_serializing)]
                private_value: String,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                optional_value: Option<String>,
                #[serde(skip_serializing_if = "Vec::is_empty")]
                labels: Vec<String>,
            }
        };
        let fields = fields_from_struct(&item.fields, &item.attrs).unwrap();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].name, "plainName");
        assert!(!fields[0].response_optional);
        assert!(fields[1].skip_serializing);
        assert_eq!(fields[2].name, "optionalValue");
        assert!(fields[2].response_optional);
        assert!(fields[2].input_optional);
        assert_eq!(fields[3].name, "labels");
        assert!(fields[3].response_optional);
    }

    #[test]
    fn unsupported_skip_serializing_predicate_fails_closed() {
        let item: syn::ItemStruct = syn::parse_quote! {
            struct Example {
                #[serde(skip_serializing_if = "custom_policy")]
                value: String,
            }
        };
        assert!(
            fields_from_struct(&item.fields, &item.attrs)
                .err()
                .unwrap()
                .contains("unsupported skip_serializing_if")
        );
    }

    #[test]
    fn serde_flatten_is_rejected_instead_of_emitting_a_wrong_field() {
        let item: syn::ItemStruct = syn::parse_quote! {
            struct Example {
                #[serde(flatten)]
                nested: Nested,
            }
        };
        assert!(
            fields_from_struct(&item.fields, &item.attrs)
                .err()
                .unwrap()
                .contains("serde(flatten)")
        );
    }

    #[test]
    fn serde_enum_tag_and_variant_names_are_rendered_as_wire_literals() {
        let item: syn::ItemEnum = syn::parse_quote! {
            #[derive(serde::Serialize)]
            #[serde(rename_all = "snake_case", tag = "type")]
            enum Choice {
                Existing { session_id: String },
                New,
            }
        };
        let context = RustTypeGraph::test_context();
        let items = vec![Item::Enum(item)];
        let mut graph = RustTypeGraph::default();
        graph.collect_items(&items, &context).unwrap();
        graph
            .emit_definition("test_crate::Choice", TypeUse::Response)
            .unwrap();
        let declaration = graph.declarations();
        assert!(declaration.contains("{ type: 'existing'; session_id: string }"));
        assert!(declaration.contains("{ type: 'new' }"));
        assert_eq!(
            apply_case("SessionCreated", Some("snake_case")).unwrap(),
            "session_created"
        );
        assert_eq!(
            apply_case("SessionCompleted", Some("camelCase")).unwrap(),
            "sessionCompleted"
        );
        assert_eq!(quote_ts_string("session_created"), "'session_created'");
    }

    #[test]
    fn plain_string_enums_export_runtime_values_from_the_serde_vocabulary() {
        let item: syn::ItemEnum = syn::parse_quote! {
            #[derive(serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            enum InteractionKind {
                Ask,
                Confirm,
                ScheduledConfirm,
            }
        };
        let context = RustTypeGraph::test_context();
        let mut graph = RustTypeGraph::default();
        graph.collect_items(&[Item::Enum(item)], &context).unwrap();
        graph
            .emit_definition("test_crate::InteractionKind", TypeUse::Response)
            .unwrap();

        let declaration = graph.declarations();
        assert!(declaration.contains(
            "export const INTERACTION_KIND_VALUES = ['ask', 'confirm', 'scheduled_confirm'] as const;"
        ));
        assert!(
            declaration.contains(
                "export type InteractionKind = (typeof INTERACTION_KIND_VALUES)[number];"
            )
        );
    }

    #[test]
    fn unsupported_serde_rename_rule_is_an_error() {
        assert!(
            apply_case("some_field", Some("not_a_serde_rule"))
                .unwrap_err()
                .contains("unsupported serde rename rule")
        );
    }

    #[test]
    fn resolver_uses_explicit_import_when_short_dto_names_are_ambiguous() {
        let mut graph = RustTypeGraph::default();
        for crate_name in ["crate_a", "crate_b"] {
            let context = TypeContext {
                crate_name: crate_name.into(),
                module: vec!["wire".into()],
                imports: HashMap::new(),
            };
            graph
                .insert_definition(&context, "SharedDto", Vec::new(), &[], true, true)
                .unwrap();
        }

        let importer = TypeContext {
            crate_name: "consumer".into(),
            module: vec!["commands".into()],
            imports: HashMap::from([(
                "ChosenDto".into(),
                vec!["crate_b".into(), "wire".into(), "SharedDto".into()],
            )]),
        };
        let imported_path = syn::parse_str::<syn::Path>("ChosenDto").unwrap();
        assert_eq!(
            graph.resolve_type(&imported_path, &importer).unwrap(),
            "crate_b::wire::SharedDto"
        );

        let ambiguous_path = syn::parse_str::<syn::Path>("SharedDto").unwrap();
        assert!(
            graph
                .resolve_type(&ambiguous_path, &importer)
                .unwrap_err()
                .contains("ambiguous Rust DTO type")
        );
    }

    #[test]
    fn unknown_paths_fail_closed_and_only_json_value_maps_to_unknown() {
        let context = TypeContext {
            crate_name: "consumer".into(),
            module: Vec::new(),
            imports: HashMap::from([("Value".into(), vec!["serde_json".into(), "Value".into()])]),
        };
        let mut graph = RustTypeGraph::default();
        let dynamic = syn::parse_str::<Type>("Value").unwrap();
        assert_eq!(
            graph
                .map_type(&dynamic, TypeUse::Response, &context)
                .unwrap()
                .ts,
            "unknown"
        );
        let missing = syn::parse_str::<Type>("MissingDto").unwrap();
        assert!(
            graph
                .map_type(&missing, TypeUse::Response, &context)
                .unwrap_err()
                .contains("unsupported/unmapped Rust DTO type")
        );
    }

    #[test]
    fn request_dtos_model_container_field_defaults_and_option_recursively() {
        let context = RustTypeGraph::test_context();
        let child: syn::ItemStruct = syn::parse_quote! {
            #[derive(serde::Serialize, serde::Deserialize)]
            struct Child {
                id: String,
                #[serde(default)]
                label: String,
                maybe: Option<String>,
            }
        };
        let parent: syn::ItemStruct = syn::parse_quote! {
            #[derive(serde::Serialize, serde::Deserialize)]
            #[serde(default)]
            struct Parent {
                child: Child,
                name: String,
            }
        };
        let mut graph = RustTypeGraph::default();
        graph
            .collect_items(&[Item::Struct(child), Item::Struct(parent)], &context)
            .unwrap();

        graph
            .emit_definition("test_crate::Parent", TypeUse::Response)
            .unwrap();
        graph
            .emit_definition("test_crate::Parent", TypeUse::Request)
            .unwrap();
        let declarations = graph.declarations();
        assert!(declarations.contains("export interface Parent { child: Child; name: string }"));
        assert!(
            declarations
                .contains("export interface ParentInput { child?: ChildInput; name?: string }")
        );
        assert!(declarations.contains(
            "export interface Child { id: string; label: string; maybe: string | null }"
        ));
        assert!(declarations.contains(
            "export interface ChildInput { id: string; label?: string; maybe?: string | null }"
        ));
    }

    #[test]
    fn request_and_response_honor_skip_direction_independently() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[derive(serde::Serialize, serde::Deserialize)]
            struct Directional {
                required: String,
                #[serde(default)]
                defaulted: String,
                #[serde(skip_deserializing)]
                output_only: String,
                #[serde(skip_serializing)]
                input_only: String,
                #[serde(skip)]
                hidden: String,
            }
        };
        let context = RustTypeGraph::test_context();
        let mut graph = RustTypeGraph::default();
        graph
            .collect_items(&[Item::Struct(item)], &context)
            .unwrap();
        graph
            .emit_definition("test_crate::Directional", TypeUse::Response)
            .unwrap();
        graph
            .emit_definition("test_crate::Directional", TypeUse::Request)
            .unwrap();
        let declarations = graph.declarations();
        assert!(declarations.contains(
            "export interface Directional { required: string; defaulted: string; output_only: string }"
        ));
        assert!(declarations.contains(
            "export interface DirectionalInput { required: string; defaulted?: string; input_only: string }"
        ));
        assert!(!declarations.contains("hidden:"));
    }

    #[test]
    fn request_export_requires_the_rust_deserialize_implementation() {
        let item: syn::ItemStruct = syn::parse_quote! {
            #[derive(serde::Serialize)]
            struct ResponseOnly { id: String }
        };
        let context = RustTypeGraph::test_context();
        let mut graph = RustTypeGraph::default();
        graph
            .collect_items(&[Item::Struct(item)], &context)
            .unwrap();
        assert!(
            graph
                .emit_definition("test_crate::ResponseOnly", TypeUse::Request)
                .unwrap_err()
                .contains("does not derive Deserialize")
        );
    }

    #[test]
    fn platform_cfg_aliases_without_serde_are_not_duplicate_ipc_dtos() {
        let windows: syn::ItemType = syn::parse_quote! {
            #[cfg(windows)]
            type ChildProcessHandle = std::os::windows::io::RawHandle;
        };
        let other: syn::ItemType = syn::parse_quote! {
            #[cfg(not(windows))]
            type ChildProcessHandle = ();
        };
        let context = RustTypeGraph::test_context();
        let mut graph = RustTypeGraph::default();

        graph
            .collect_items(&[Item::Type(windows), Item::Type(other)], &context)
            .unwrap();

        assert!(graph.definitions.is_empty());
    }

    #[test]
    fn settings_macro_container_default_makes_only_input_shape_partial() {
        let item: Item = syn::parse_quote! {
            settings_pair! {
                settings;
                default_shell: ShellChoice,
                description: Option<String>;
                sanitize
            }
        };
        let context = RustTypeGraph::test_context();
        let mut graph = RustTypeGraph::default();
        graph.collect_items(&[item], &context).unwrap();
        graph
            .insert_alias(
                &context,
                "ShellChoice",
                syn::parse_quote!(String),
                &[],
                true,
                true,
            )
            .unwrap();
        graph
            .emit_definition("test_crate::Settings", TypeUse::Response)
            .unwrap();
        graph
            .emit_definition("test_crate::Settings", TypeUse::Request)
            .unwrap();
        let declarations = graph.declarations();
        assert!(declarations.contains(
            "export interface Settings { default_shell: ShellChoice; description: string | null }"
        ));
        assert!(declarations.contains(
            "export interface SettingsInput { default_shell?: ShellChoiceInput; description?: string | null }"
        ));
    }

    #[test]
    fn custom_deserializer_metadata_fails_closed_for_input_generation() {
        assert!(
            serde_options(&[syn::parse_quote!(#[serde(deserialize_with = "custom")])])
                .unwrap_err()
                .contains("custom Serde wire behavior")
        );
        assert!(
            serde_options(&[syn::parse_quote!(#[serde(future_wire_mode)])])
                .unwrap_err()
                .contains("unsupported serde metadata")
        );
    }
}
