//! DAG Execution Engine for step dependency resolution.
//!
//! Computes execution layers from step dependencies using topological sort (Kahn's algorithm).
//! Steps are organized into layers where all steps in a layer can execute in parallel.

use std::collections::{HashMap, HashSet, VecDeque};

use super::executor_types::ExecutionStepConfig;

/// Compute execution layers from step dependencies using topological sort (Kahn's algorithm).
///
/// Steps are organized into layers where all steps in a layer can execute in parallel.
/// Dependencies come from two sources:
/// 1. `inputs` — referenced step IDs are implicit dependencies
/// 2. `depends_on` — explicit ordering constraints
///
/// Returns `Vec<Vec<usize>>` where each inner vec is a layer of step indices that
/// can run concurrently. Returns Err if a cycle is detected.
pub fn compute_execution_layers(steps: &[ExecutionStepConfig]) -> Result<Vec<Vec<usize>>, String> {
    let n = steps.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    // Build a map from step ID to index
    let mut id_to_index: HashMap<String, usize> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        if let Some(ref id) = step.id {
            id_to_index.insert(id.clone(), i);
        }
    }

    // Build adjacency list and in-degree counts
    let mut in_degree = vec![0usize; n];
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, step) in steps.iter().enumerate() {
        // Collect dependencies from `inputs` (extract referenced step IDs)
        if let Some(ref inputs) = step.inputs {
            for reference in inputs.values() {
                // Parse "step-id.property[.path]" — extract the step ID (first segment)
                let step_id = reference.split('.').next().unwrap_or("");
                if let Some(&dep_index) = id_to_index.get(step_id) {
                    if dep_index != i {
                        adjacency[dep_index].push(i);
                        in_degree[i] += 1;
                    }
                }
            }
        }

        // Collect dependencies from `depends_on` (explicit ordering)
        if let Some(ref deps) = step.depends_on {
            for dep_id in deps {
                if let Some(&dep_index) = id_to_index.get(dep_id) {
                    if dep_index != i {
                        adjacency[dep_index].push(i);
                        in_degree[i] += 1;
                    }
                }
            }
        }
    }

    // Kahn's algorithm: BFS topological sort, grouping into layers
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate().take(n) {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut layers: Vec<Vec<usize>> = Vec::new();
    let mut processed = 0;

    while !queue.is_empty() {
        let layer_size = queue.len();
        let mut layer = Vec::with_capacity(layer_size);

        for _ in 0..layer_size {
            let node = queue.pop_front().unwrap();
            layer.push(node);
            processed += 1;

            for &neighbor in &adjacency[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    queue.push_back(neighbor);
                }
            }
        }

        layers.push(layer);
    }

    if processed != n {
        return Err(format!(
            "Circular dependency detected: {} of {} steps could not be ordered",
            n - processed,
            n
        ));
    }

    Ok(layers)
}

/// Extract step IDs referenced in an `inputs` map.
///
/// Each input value has format "step-id.property[.path]".
/// Returns the unique set of referenced step IDs.
pub fn extract_input_dependencies(inputs: &HashMap<String, String>) -> HashSet<String> {
    inputs
        .values()
        .filter_map(|reference| {
            let step_id = reference.split('.').next()?;
            if step_id.is_empty() {
                None
            } else {
                Some(step_id.to_string())
            }
        })
        .collect()
}
