//! Bounded, declarative workflow definitions.
//!
//! This module validates graph structure and resolves explicit result-field
//! references. It never executes a capability or grants permission to do so.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The only workflow definition version accepted by this build.
pub(crate) const WORKFLOW_DEFINITION_VERSION: u16 = 1;

/// Resource ceilings for one inline workflow. Callers may lower these limits;
/// model-controlled input cannot raise the process ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkflowLimits {
    pub max_definition_bytes: usize,
    pub max_steps: usize,
    pub max_dependency_depth: usize,
    pub max_step_id_bytes: usize,
    pub max_capability_name_bytes: usize,
    pub max_dependencies_per_step: usize,
    pub max_mapping_nodes: usize,
    pub max_mapping_depth: usize,
    pub max_result_pointer_bytes: usize,
    pub max_result_pointer_segments: usize,
    pub max_step_input_bytes: usize,
    pub max_total_input_bytes: usize,
    pub max_step_output_bytes: usize,
    pub max_total_output_bytes: usize,
    pub max_final_observation_bytes: usize,
    pub max_execution_duration_secs: u64,
}

impl Default for WorkflowLimits {
    fn default() -> Self {
        Self {
            max_definition_bytes: 64 * 1024,
            max_steps: 16,
            max_dependency_depth: 16,
            max_step_id_bytes: 48,
            max_capability_name_bytes: 192,
            max_dependencies_per_step: 16,
            max_mapping_nodes: 512,
            max_mapping_depth: 16,
            max_result_pointer_bytes: 256,
            max_result_pointer_segments: 16,
            max_step_input_bytes: 16 * 1024,
            max_total_input_bytes: 64 * 1024,
            max_step_output_bytes: 16 * 1024,
            max_total_output_bytes: 64 * 1024,
            max_final_observation_bytes: 32 * 1024,
            max_execution_duration_secs: 90,
        }
    }
}

impl WorkflowLimits {
    fn clamp_to_process_ceiling(mut self) -> Self {
        let ceiling = Self::default();
        self.max_definition_bytes = self.max_definition_bytes.min(ceiling.max_definition_bytes);
        self.max_steps = self.max_steps.min(ceiling.max_steps);
        self.max_dependency_depth = self.max_dependency_depth.min(ceiling.max_dependency_depth);
        self.max_step_id_bytes = self.max_step_id_bytes.min(ceiling.max_step_id_bytes);
        self.max_capability_name_bytes = self
            .max_capability_name_bytes
            .min(ceiling.max_capability_name_bytes);
        self.max_dependencies_per_step = self
            .max_dependencies_per_step
            .min(ceiling.max_dependencies_per_step);
        self.max_mapping_nodes = self.max_mapping_nodes.min(ceiling.max_mapping_nodes);
        self.max_mapping_depth = self.max_mapping_depth.min(ceiling.max_mapping_depth);
        self.max_result_pointer_bytes = self
            .max_result_pointer_bytes
            .min(ceiling.max_result_pointer_bytes);
        self.max_result_pointer_segments = self
            .max_result_pointer_segments
            .min(ceiling.max_result_pointer_segments);
        self.max_step_input_bytes = self.max_step_input_bytes.min(ceiling.max_step_input_bytes);
        self.max_total_input_bytes = self
            .max_total_input_bytes
            .min(ceiling.max_total_input_bytes);
        self.max_step_output_bytes = self
            .max_step_output_bytes
            .min(ceiling.max_step_output_bytes);
        self.max_total_output_bytes = self
            .max_total_output_bytes
            .min(ceiling.max_total_output_bytes);
        self.max_final_observation_bytes = self
            .max_final_observation_bytes
            .min(ceiling.max_final_observation_bytes);
        self.max_execution_duration_secs = self
            .max_execution_duration_secs
            .min(ceiling.max_execution_duration_secs);
        self
    }
}

/// A serialized workflow accepted by `workflow_run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowDefinition {
    pub version: u16,
    pub steps: Vec<WorkflowStep>,
}

/// One exact capability invocation. Edges are declared in `depends_on` or
/// inferred from `WorkflowValue::Result` references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowStep {
    pub id: String,
    pub capability: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub input: BTreeMap<String, WorkflowValue>,
}

/// Closed input mapping language: JSON literals, nested maps/lists, and an
/// explicit JSON Pointer into an earlier step's completed output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkflowValue {
    Literal {
        value: Value,
    },
    Object {
        fields: BTreeMap<String, WorkflowValue>,
    },
    Array {
        items: Vec<WorkflowValue>,
    },
    Result {
        step_id: String,
        json_pointer: String,
    },
}

/// A validated deterministic execution order. It is still only a plan:
/// authorization and live capability checks are required for every step.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ValidatedWorkflow {
    steps: Vec<WorkflowStep>,
    limits: WorkflowLimits,
}

impl ValidatedWorkflow {
    pub(crate) fn steps(&self) -> &[WorkflowStep] {
        &self.steps
    }

    pub(crate) fn limits(&self) -> WorkflowLimits {
        self.limits
    }

    /// Materialize one step's arguments from outputs already completed by the
    /// caller. Missing values and oversize resolved inputs fail closed.
    pub(crate) fn resolve_input(
        &self,
        step_id: &str,
        completed_outputs: &HashMap<String, Value>,
    ) -> Result<Value, WorkflowValidationError> {
        let step = self
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .ok_or(WorkflowValidationError::UnknownStep)?;
        let mut input = Map::new();
        for (name, mapping) in &step.input {
            input.insert(name.clone(), resolve_value(mapping, completed_outputs)?);
        }
        let input = Value::Object(input);
        let size = serde_json::to_vec(&input)
            .map_err(|_| WorkflowValidationError::InvalidDefinition)?
            .len();
        if size > self.limits.max_step_input_bytes {
            return Err(WorkflowValidationError::StepInputTooLarge {
                limit: self.limits.max_step_input_bytes,
            });
        }
        Ok(input)
    }
}

/// Stable validation failures. Messages intentionally omit model-provided
/// capability names, values, and resource contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WorkflowValidationError {
    #[error("workflow definition is invalid")]
    InvalidDefinition,
    #[error("workflow definition version is unsupported")]
    UnsupportedVersion,
    #[error("workflow definition exceeds the {limit}-byte limit")]
    DefinitionTooLarge { limit: usize },
    #[error("workflow must contain between one and {limit} steps")]
    InvalidStepCount { limit: usize },
    #[error("workflow step identity is invalid")]
    InvalidStepId,
    #[error("workflow step identities must be unique")]
    DuplicateStepId,
    #[error("workflow capability identity is invalid")]
    InvalidCapability,
    #[error("workflow references a capability outside the current allowlist")]
    CapabilityNotAllowed,
    #[error("workflow has too many dependencies on one step")]
    TooManyDependencies,
    #[error("workflow contains an invalid or repeated dependency")]
    InvalidDependency,
    #[error("workflow mapping exceeds the node or nesting limit")]
    MappingTooComplex,
    #[error("workflow contains an invalid result reference")]
    InvalidResultReference,
    #[error("workflow result reference is not a valid bounded JSON Pointer")]
    InvalidResultPointer,
    #[error("workflow contains a dependency cycle")]
    DependencyCycle,
    #[error("workflow dependency depth exceeds the {limit}-step limit")]
    DependencyDepthExceeded { limit: usize },
    #[error("workflow references an unknown step")]
    UnknownStep,
    #[error("workflow result reference is unavailable")]
    ResultUnavailable,
    #[error("workflow step input exceeds the {limit}-byte limit")]
    StepInputTooLarge { limit: usize },
}

/// Validate against capability names taken from the current trusted catalog
/// snapshot. The allowlist is data supplied by the host, never the model.
pub(crate) fn validate_workflow(
    definition: &WorkflowDefinition,
    allowed_capabilities: &BTreeSet<String>,
    requested_limits: WorkflowLimits,
) -> Result<ValidatedWorkflow, WorkflowValidationError> {
    let limits = requested_limits.clamp_to_process_ceiling();
    let size = serde_json::to_vec(definition)
        .map_err(|_| WorkflowValidationError::InvalidDefinition)?
        .len();
    if size > limits.max_definition_bytes {
        return Err(WorkflowValidationError::DefinitionTooLarge {
            limit: limits.max_definition_bytes,
        });
    }
    if definition.version != WORKFLOW_DEFINITION_VERSION {
        return Err(WorkflowValidationError::UnsupportedVersion);
    }
    if definition.steps.is_empty() || definition.steps.len() > limits.max_steps {
        return Err(WorkflowValidationError::InvalidStepCount {
            limit: limits.max_steps,
        });
    }

    let mut ids = BTreeMap::<&str, usize>::new();
    for (index, step) in definition.steps.iter().enumerate() {
        if !valid_step_id(&step.id, limits.max_step_id_bytes) {
            return Err(WorkflowValidationError::InvalidStepId);
        }
        if ids.insert(&step.id, index).is_some() {
            return Err(WorkflowValidationError::DuplicateStepId);
        }
        if step.capability.is_empty()
            || step.capability.len() > limits.max_capability_name_bytes
            || step.capability.chars().any(char::is_control)
        {
            return Err(WorkflowValidationError::InvalidCapability);
        }
        if !allowed_capabilities.contains(&step.capability) {
            return Err(WorkflowValidationError::CapabilityNotAllowed);
        }
        if step.depends_on.len() > limits.max_dependencies_per_step {
            return Err(WorkflowValidationError::TooManyDependencies);
        }
    }

    let mut dependencies = vec![BTreeSet::<usize>::new(); definition.steps.len()];
    let mut mapping_nodes = 0usize;
    for (index, step) in definition.steps.iter().enumerate() {
        for dependency in &step.depends_on {
            let Some(dependency_index) = ids.get(dependency.as_str()).copied() else {
                return Err(WorkflowValidationError::InvalidDependency);
            };
            if dependency_index == index || !dependencies[index].insert(dependency_index) {
                return Err(WorkflowValidationError::InvalidDependency);
            }
        }
        for mapping in step.input.values() {
            collect_dependencies(
                mapping,
                index,
                0,
                &ids,
                &mut dependencies[index],
                &mut mapping_nodes,
                limits,
            )?;
        }
    }

    let ordered_indices = topological_order(&dependencies, definition, limits)?;
    let steps = ordered_indices
        .into_iter()
        .map(|index| definition.steps[index].clone())
        .collect();
    Ok(ValidatedWorkflow { steps, limits })
}

fn valid_step_id(id: &str, max_bytes: usize) -> bool {
    if id.is_empty() || id.len() > max_bytes {
        return false;
    }
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

#[allow(clippy::too_many_arguments)]
fn collect_dependencies(
    value: &WorkflowValue,
    step_index: usize,
    depth: usize,
    ids: &BTreeMap<&str, usize>,
    dependencies: &mut BTreeSet<usize>,
    mapping_nodes: &mut usize,
    limits: WorkflowLimits,
) -> Result<(), WorkflowValidationError> {
    *mapping_nodes = mapping_nodes.saturating_add(1);
    if *mapping_nodes > limits.max_mapping_nodes || depth > limits.max_mapping_depth {
        return Err(WorkflowValidationError::MappingTooComplex);
    }
    match value {
        WorkflowValue::Literal { .. } => {}
        WorkflowValue::Object { fields } => {
            for child in fields.values() {
                collect_dependencies(
                    child,
                    step_index,
                    depth + 1,
                    ids,
                    dependencies,
                    mapping_nodes,
                    limits,
                )?;
            }
        }
        WorkflowValue::Array { items } => {
            for child in items {
                collect_dependencies(
                    child,
                    step_index,
                    depth + 1,
                    ids,
                    dependencies,
                    mapping_nodes,
                    limits,
                )?;
            }
        }
        WorkflowValue::Result {
            step_id,
            json_pointer,
        } => {
            let Some(dependency_index) = ids.get(step_id.as_str()).copied() else {
                return Err(WorkflowValidationError::InvalidResultReference);
            };
            if dependency_index == step_index {
                return Err(WorkflowValidationError::InvalidResultReference);
            }
            validate_json_pointer(json_pointer, limits)?;
            dependencies.insert(dependency_index);
        }
    }
    Ok(())
}

fn validate_json_pointer(
    pointer: &str,
    limits: WorkflowLimits,
) -> Result<(), WorkflowValidationError> {
    if pointer.is_empty()
        || pointer.len() > limits.max_result_pointer_bytes
        || !pointer.starts_with('/')
    {
        return Err(WorkflowValidationError::InvalidResultPointer);
    }
    let mut segments = 0usize;
    for segment in pointer[1..].split('/') {
        segments += 1;
        if segments > limits.max_result_pointer_segments {
            return Err(WorkflowValidationError::InvalidResultPointer);
        }
        let mut chars = segment.chars();
        while let Some(character) = chars.next() {
            if character == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err(WorkflowValidationError::InvalidResultPointer);
            }
        }
    }
    Ok(())
}

fn topological_order(
    dependencies: &[BTreeSet<usize>],
    definition: &WorkflowDefinition,
    limits: WorkflowLimits,
) -> Result<Vec<usize>, WorkflowValidationError> {
    let mut dependents = vec![Vec::<usize>::new(); dependencies.len()];
    let mut remaining: Vec<_> = dependencies.iter().map(BTreeSet::len).collect();
    for (step_index, step_dependencies) in dependencies.iter().enumerate() {
        for dependency_index in step_dependencies {
            dependents[*dependency_index].push(step_index);
        }
    }

    let mut ready = BTreeSet::<(String, usize)>::new();
    for (index, count) in remaining.iter().enumerate() {
        if *count == 0 {
            ready.insert((definition.steps[index].id.clone(), index));
        }
    }

    let mut ordered = Vec::with_capacity(dependencies.len());
    let mut depth = vec![1usize; dependencies.len()];
    while let Some((_, index)) = ready.pop_first() {
        ordered.push(index);
        if depth[index] > limits.max_dependency_depth {
            return Err(WorkflowValidationError::DependencyDepthExceeded {
                limit: limits.max_dependency_depth,
            });
        }
        for dependent in &dependents[index] {
            depth[*dependent] = depth[*dependent].max(depth[index].saturating_add(1));
            remaining[*dependent] = remaining[*dependent].saturating_sub(1);
            if remaining[*dependent] == 0 {
                ready.insert((definition.steps[*dependent].id.clone(), *dependent));
            }
        }
    }
    if ordered.len() != dependencies.len() {
        return Err(WorkflowValidationError::DependencyCycle);
    }
    Ok(ordered)
}

fn resolve_value(
    mapping: &WorkflowValue,
    completed_outputs: &HashMap<String, Value>,
) -> Result<Value, WorkflowValidationError> {
    match mapping {
        WorkflowValue::Literal { value } => Ok(value.clone()),
        WorkflowValue::Object { fields } => {
            let mut object = Map::new();
            for (name, value) in fields {
                object.insert(name.clone(), resolve_value(value, completed_outputs)?);
            }
            Ok(Value::Object(object))
        }
        WorkflowValue::Array { items } => items
            .iter()
            .map(|item| resolve_value(item, completed_outputs))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        WorkflowValue::Result {
            step_id,
            json_pointer,
        } => {
            let output = completed_outputs
                .get(step_id)
                .ok_or(WorkflowValidationError::ResultUnavailable)?;
            resolve_json_pointer(output, json_pointer)
                .cloned()
                .ok_or(WorkflowValidationError::ResultUnavailable)
        }
    }
}

fn resolve_json_pointer<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in pointer.strip_prefix('/')?.split('/') {
        let mut decoded = String::with_capacity(segment.len());
        let mut chars = segment.chars();
        while let Some(character) = chars.next() {
            if character == '~' {
                decoded.push(match chars.next()? {
                    '0' => '~',
                    '1' => '/',
                    _ => return None,
                });
            } else {
                decoded.push(character);
            }
        }
        current = match current {
            Value::Object(object) => object.get(&decoded)?,
            Value::Array(items) => {
                if decoded.is_empty()
                    || (decoded.len() > 1 && decoded.starts_with('0'))
                    || !decoded.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return None;
                }
                items.get(decoded.parse::<usize>().ok()?)?
            }
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn allowed() -> BTreeSet<String> {
        BTreeSet::from(["files.read".into(), "text.summarize".into()])
    }

    fn step(id: &str, capability: &str) -> WorkflowStep {
        WorkflowStep {
            id: id.into(),
            capability: capability.into(),
            depends_on: Vec::new(),
            input: BTreeMap::new(),
        }
    }

    fn workflow(steps: Vec<WorkflowStep>) -> WorkflowDefinition {
        WorkflowDefinition {
            version: WORKFLOW_DEFINITION_VERSION,
            steps,
        }
    }

    #[test]
    fn validates_reference_dependencies_and_uses_stable_topological_order() {
        let mut first = step("read", "files.read");
        first.input.insert(
            "path".into(),
            WorkflowValue::Literal {
                value: json!("notes.txt"),
            },
        );
        let mut second = step("summarize", "text.summarize");
        second.input.insert(
            "text".into(),
            WorkflowValue::Result {
                step_id: "read".into(),
                json_pointer: "/content/text".into(),
            },
        );
        let validated = validate_workflow(
            &workflow(vec![second, first]),
            &allowed(),
            WorkflowLimits::default(),
        )
        .unwrap();
        assert_eq!(
            validated
                .steps()
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            ["read", "summarize"]
        );
    }

    #[test]
    fn resolves_nested_literals_and_explicit_result_fields() {
        let mut summarize = step("summarize", "text.summarize");
        summarize.input.insert(
            "request".into(),
            WorkflowValue::Object {
                fields: BTreeMap::from([
                    (
                        "prompt".into(),
                        WorkflowValue::Literal {
                            value: json!("Give a short summary"),
                        },
                    ),
                    (
                        "text".into(),
                        WorkflowValue::Result {
                            step_id: "read".into(),
                            json_pointer: "/content~1raw".into(),
                        },
                    ),
                ]),
            },
        );
        let plan = validate_workflow(
            &workflow(vec![step("read", "files.read"), summarize]),
            &allowed(),
            WorkflowLimits::default(),
        )
        .unwrap();
        let results = HashMap::from([("read".into(), json!({"content/raw": "untrusted text"}))]);
        assert_eq!(
            plan.resolve_input("summarize", &results).unwrap(),
            json!({"request": {"prompt": "Give a short summary", "text": "untrusted text"}})
        );
    }

    #[test]
    fn rejects_unknown_versions_capabilities_and_cycles_and_orders_forward_dependencies() {
        let mut unsupported = workflow(vec![step("one", "files.read")]);
        unsupported.version = 2;
        assert_eq!(
            validate_workflow(&unsupported, &allowed(), WorkflowLimits::default()),
            Err(WorkflowValidationError::UnsupportedVersion)
        );
        assert_eq!(
            validate_workflow(
                &workflow(vec![step("one", "admin.restart")]),
                &allowed(),
                WorkflowLimits::default()
            ),
            Err(WorkflowValidationError::CapabilityNotAllowed)
        );

        let mut first = step("first", "files.read");
        first.depends_on.push("second".into());
        let mut second = step("second", "text.summarize");
        second.input.insert(
            "value".into(),
            WorkflowValue::Result {
                step_id: "first".into(),
                json_pointer: "/value".into(),
            },
        );
        assert_eq!(
            validate_workflow(
                &workflow(vec![first, second]),
                &allowed(),
                WorkflowLimits::default()
            ),
            Err(WorkflowValidationError::DependencyCycle)
        );

        let mut forward = step("first", "files.read");
        forward.input.insert(
            "value".into(),
            WorkflowValue::Result {
                step_id: "later".into(),
                json_pointer: "/value".into(),
            },
        );
        let plan = validate_workflow(
            &workflow(vec![forward, step("later", "text.summarize")]),
            &allowed(),
            WorkflowLimits::default(),
        )
        .unwrap();
        assert_eq!(
            plan.steps()
                .iter()
                .map(|step| step.id.as_str())
                .collect::<Vec<_>>(),
            ["later", "first"]
        );
    }

    #[test]
    fn rejects_invalid_graph_shape_mapping_and_wire_extensions() {
        assert_eq!(
            validate_workflow(&workflow(vec![]), &allowed(), WorkflowLimits::default()),
            Err(WorkflowValidationError::InvalidStepCount { limit: 16 })
        );
        assert_eq!(
            validate_workflow(
                &workflow(vec![step("Has Space", "files.read")]),
                &allowed(),
                WorkflowLimits::default()
            ),
            Err(WorkflowValidationError::InvalidStepId)
        );

        let encoded = json!({
            "version": 1,
            "steps": [{"id": "one", "capability": "files.read", "input": {}}],
            "retry": true
        });
        assert!(serde_json::from_value::<WorkflowDefinition>(encoded).is_err());

        let mut invalid_pointer = step("read", "files.read");
        invalid_pointer.input.insert(
            "field".into(),
            WorkflowValue::Result {
                step_id: "prior".into(),
                json_pointer: "/field~2name".into(),
            },
        );
        assert_eq!(
            validate_workflow(
                &workflow(vec![step("prior", "files.read"), invalid_pointer]),
                &allowed(),
                WorkflowLimits::default()
            ),
            Err(WorkflowValidationError::InvalidResultPointer)
        );
    }

    #[test]
    fn fails_closed_when_result_or_resolved_input_is_missing_or_oversize() {
        let mut summarize = step("summarize", "text.summarize");
        summarize.input.insert(
            "text".into(),
            WorkflowValue::Result {
                step_id: "read".into(),
                json_pointer: "/text".into(),
            },
        );
        let plan = validate_workflow(
            &workflow(vec![step("read", "files.read"), summarize]),
            &allowed(),
            WorkflowLimits::default(),
        )
        .unwrap();
        assert_eq!(
            plan.resolve_input("summarize", &HashMap::new()),
            Err(WorkflowValidationError::ResultUnavailable)
        );

        let small = WorkflowLimits {
            max_step_input_bytes: 8,
            ..WorkflowLimits::default()
        };
        let plan = validate_workflow(
            &workflow(vec![{
                let mut large = step("large", "files.read");
                large.input.insert(
                    "value".into(),
                    WorkflowValue::Literal {
                        value: json!("0123456789"),
                    },
                );
                large
            }]),
            &allowed(),
            small,
        )
        .unwrap();
        assert_eq!(
            plan.resolve_input("large", &HashMap::new()),
            Err(WorkflowValidationError::StepInputTooLarge { limit: 8 })
        );
    }

    #[test]
    fn limits_cannot_be_raised_and_definition_bytes_are_bounded() {
        let requested = WorkflowLimits {
            max_steps: usize::MAX,
            max_total_output_bytes: usize::MAX,
            max_execution_duration_secs: u64::MAX,
            ..WorkflowLimits::default()
        };
        let invalid_count = workflow(
            (0..17)
                .map(|index| step(&format!("step{index}"), "files.read"))
                .collect(),
        );
        assert_eq!(
            validate_workflow(&invalid_count, &allowed(), requested),
            Err(WorkflowValidationError::InvalidStepCount { limit: 16 })
        );
        let plan = validate_workflow(
            &workflow(vec![step("one", "files.read")]),
            &allowed(),
            requested,
        )
        .unwrap();
        assert_eq!(plan.limits().max_total_output_bytes, 64 * 1024);
        assert_eq!(plan.limits().max_execution_duration_secs, 90);

        let small = WorkflowLimits {
            max_definition_bytes: 64,
            ..WorkflowLimits::default()
        };
        assert!(matches!(
            validate_workflow(
                &workflow(vec![step("large", "files.read")]),
                &allowed(),
                small
            ),
            Err(WorkflowValidationError::DefinitionTooLarge { limit: 64 })
        ));
    }
}
