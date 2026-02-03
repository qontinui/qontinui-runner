//! Python Circular Dependency Detector
//!
//! Detects circular imports in Python code using tree-sitter for parsing.
//! Uses Tarjan's algorithm for finding strongly connected components (SCCs).

#![allow(dead_code)]

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{debug, warn};

/// Represents a module in the dependency graph
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct ModulePath(String);

impl ModulePath {
    fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Import information extracted from a Python file
#[derive(Debug, Clone)]
struct ImportInfo {
    /// The module being imported (e.g., "foo.bar")
    module: String,
    /// Line number where the import occurs
    line: u32,
    /// Whether this is a relative import
    is_relative: bool,
    /// Number of leading dots for relative imports
    relative_level: u32,
}

/// Dependency graph node
struct DependencyNode {
    /// File path for error reporting
    file_path: String,
    /// List of imports with their line numbers
    imports: Vec<ImportInfo>,
}

/// Main analyze function - entry point for the circular dependency detector
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
    let root_path = Path::new(working_dir);
    if !root_path.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    let config = WalkConfig {
        extensions: vec!["py".to_string()],
        ..Default::default()
    };

    let python_files = walk_files(root_path, &config);
    debug!("Found {} Python files to analyze", python_files.len());

    if python_files.is_empty() {
        return Ok(ParsedOutput {
            issues_found: 0,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: CheckStructuredOutput {
                issues: vec![],
                summary: Some(CheckSummary {
                    total_files: 0,
                    files_with_issues: 0,
                    total_issues: 0,
                    issues_by_severity: HashMap::new(),
                }),
                tool_data: HashMap::new(),
            },
        });
    }

    // Build the dependency graph
    let mut graph: HashMap<ModulePath, DependencyNode> = HashMap::new();
    let mut module_to_file: HashMap<ModulePath, String> = HashMap::new();

    // Initialize the parser
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&language.into())
        .map_err(|e| format!("Failed to set Python language: {}", e))?;

    for file_path in &python_files {
        let file_path_str = file_path.to_string_lossy().to_string();

        // Convert file path to module path
        let module_path = file_path_to_module(root_path, file_path);

        // Read and parse the file
        let source_code = match std::fs::read_to_string(file_path) {
            Ok(content) => content,
            Err(e) => {
                warn!("Failed to read file {}: {}", file_path_str, e);
                continue;
            }
        };

        let imports = extract_imports(&mut parser, &source_code, root_path, file_path);

        module_to_file.insert(module_path.clone(), file_path_str.clone());
        graph.insert(
            module_path,
            DependencyNode {
                file_path: file_path_str,
                imports,
            },
        );
    }

    // Find all strongly connected components using Tarjan's algorithm
    let cycles = find_cycles(&graph, &module_to_file, root_path);

    // Convert cycles to issues
    let mut issues = Vec::new();
    let mut files_with_issues = HashSet::new();

    for cycle in &cycles {
        if cycle.len() > 1 {
            // Build the cycle path string
            let cycle_path: Vec<&str> = cycle.iter().map(|m| m.as_str()).collect();
            let cycle_str = format!("{} -> {}", cycle_path.join(" -> "), cycle_path[0]);

            // Get the first file in the cycle for the issue location
            if let Some(first_module) = cycle.first() {
                let file_path = module_to_file
                    .get(first_module)
                    .map(|s| s.as_str())
                    .unwrap_or("unknown");
                files_with_issues.insert(file_path.to_string());

                // Find the import line in the first module that points to the second module
                let import_line = if let (Some(node), Some(second_module)) =
                    (graph.get(first_module), cycle.get(1))
                {
                    node.imports
                        .iter()
                        .find(|imp| module_matches_import(second_module, imp, root_path))
                        .map(|imp| imp.line)
                } else {
                    None
                };

                let issue = IssueBuilder::new(
                    file_path,
                    format!("Circular dependency detected: {}", cycle_str),
                )
                .code("CIRCULAR_DEP")
                .severity(IssueSeverity::Error);

                let issue = if let Some(line) = import_line {
                    issue.line(line)
                } else {
                    issue
                };

                issues.push(issue.build());
            }
        }
    }

    let issues_found = issues.len() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: python_files.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: python_files.len() as u32,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([("error".to_string(), issues_found)]),
            }),
            tool_data: HashMap::from([
                ("total_modules".to_string(), serde_json::json!(graph.len())),
                ("cycles_found".to_string(), serde_json::json!(issues_found)),
            ]),
        },
    })
}

/// Convert a file path to a Python module path
fn file_path_to_module(root: &Path, file_path: &Path) -> ModulePath {
    let relative = file_path
        .strip_prefix(root)
        .unwrap_or(file_path)
        .with_extension("");

    let module_path = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(".");

    // Handle __init__.py files - they represent the parent package
    let module_path = if module_path.ends_with(".__init__") {
        module_path.trim_end_matches(".__init__").to_string()
    } else {
        module_path
    };

    ModulePath::new(module_path)
}

/// Extract imports from a Python source file using tree-sitter
fn extract_imports(
    parser: &mut tree_sitter::Parser,
    source: &str,
    root: &Path,
    current_file: &Path,
) -> Vec<ImportInfo> {
    let mut imports = Vec::new();

    let tree = match parser.parse(source, None) {
        Some(tree) => tree,
        None => {
            warn!("Failed to parse Python file");
            return imports;
        }
    };

    let root_node = tree.root_node();
    extract_imports_from_node(&root_node, source, root, current_file, &mut imports);

    imports
}

/// Recursively extract imports from AST nodes
fn extract_imports_from_node(
    node: &tree_sitter::Node,
    source: &str,
    root: &Path,
    current_file: &Path,
    imports: &mut Vec<ImportInfo>,
) {
    match node.kind() {
        "import_statement" => {
            // Handle: import foo, import foo.bar
            extract_regular_import(node, source, imports);
        }
        "import_from_statement" => {
            // Handle: from foo import bar, from . import bar, from ..foo import bar
            extract_from_import(node, source, root, current_file, imports);
        }
        _ => {
            // Recurse into children
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_imports_from_node(&child, source, root, current_file, imports);
            }
        }
    }
}

/// Extract imports from a regular import statement (import foo.bar)
fn extract_regular_import(node: &tree_sitter::Node, source: &str, imports: &mut Vec<ImportInfo>) {
    let line = node.start_position().row as u32 + 1;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "dotted_name" || child.kind() == "aliased_import" {
            let module_node = if child.kind() == "aliased_import" {
                child.child_by_field_name("name")
            } else {
                Some(child)
            };

            if let Some(module_node) = module_node {
                let module_text = &source[module_node.byte_range()];
                imports.push(ImportInfo {
                    module: module_text.to_string(),
                    line,
                    is_relative: false,
                    relative_level: 0,
                });
            }
        }
    }
}

/// Extract imports from a from-import statement (from foo import bar)
fn extract_from_import(
    node: &tree_sitter::Node,
    source: &str,
    root: &Path,
    current_file: &Path,
    imports: &mut Vec<ImportInfo>,
) {
    let line = node.start_position().row as u32 + 1;

    // Count leading dots for relative imports
    let mut relative_level = 0u32;
    let mut module_name: Option<String> = None;
    let mut imported_names: Vec<String> = Vec::new();

    // Try to get module_name field first (tree-sitter-python field name)
    if let Some(module_node) = node.child_by_field_name("module_name") {
        match module_node.kind() {
            "dotted_name" => {
                module_name = Some(source[module_node.byte_range()].to_string());
            }
            "relative_import" => {
                // Handle relative imports
                let mut inner_cursor = module_node.walk();
                for inner_child in module_node.children(&mut inner_cursor) {
                    match inner_child.kind() {
                        "import_prefix" => {
                            let prefix_text = &source[inner_child.byte_range()];
                            relative_level =
                                prefix_text.chars().filter(|c| *c == '.').count() as u32;
                        }
                        "dotted_name" => {
                            module_name = Some(source[inner_child.byte_range()].to_string());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Fallback: walk children if field name approach didn't work
    if module_name.is_none() && relative_level == 0 {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "import_prefix" => {
                    // Count dots
                    let prefix_text = &source[child.byte_range()];
                    relative_level = prefix_text.chars().filter(|c| *c == '.').count() as u32;
                }
                "dotted_name" => {
                    module_name = Some(source[child.byte_range()].to_string());
                }
                "relative_import" => {
                    // Handle relative imports
                    let mut inner_cursor = child.walk();
                    for inner_child in child.children(&mut inner_cursor) {
                        match inner_child.kind() {
                            "import_prefix" => {
                                let prefix_text = &source[inner_child.byte_range()];
                                relative_level =
                                    prefix_text.chars().filter(|c| *c == '.').count() as u32;
                            }
                            "dotted_name" => {
                                module_name = Some(source[inner_child.byte_range()].to_string());
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Extract the names being imported (for "from X import a, b, c")
    // Walk through all children to find imported names
    let mut cursor = node.walk();
    let mut past_module = false;

    for child in node.children(&mut cursor) {
        let kind = child.kind();

        // Skip keywords
        if kind == "from" || kind == "import" || kind == "," {
            if kind == "import" {
                past_module = true;  // Everything after "import" is an imported name
            }
            continue;
        }

        // The module specification is the first dotted_name or relative_import
        if (kind == "dotted_name" || kind == "relative_import") && !past_module {
            past_module = true;
            continue;
        }

        // After the module, collect imported names
        if past_module {
            match kind {
                "aliased_import" => {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = source[name_node.byte_range()].to_string();
                        if !imported_names.contains(&name) {
                            imported_names.push(name);
                        }
                    }
                }
                "dotted_name" | "identifier" => {
                    let name = source[child.byte_range()].to_string();
                    if !imported_names.contains(&name) {
                        imported_names.push(name);
                    }
                }
                _ => {}
            }
        }
    }

    let is_relative = relative_level > 0;

    // Resolve base module path
    let base_module = if is_relative {
        resolve_relative_import(root, current_file, relative_level, module_name.as_deref())
    } else {
        module_name.clone().unwrap_or_default()
    };

    // For "from X import Y", add both X and X.Y as potential dependencies
    // This handles cases where Y might be a submodule
    if !base_module.is_empty() {
        imports.push(ImportInfo {
            module: base_module.clone(),
            line,
            is_relative,
            relative_level,
        });

        // Also add X.Y for each imported name (they might be submodules)
        for name in &imported_names {
            let submodule = format!("{}.{}", base_module, name);
            imports.push(ImportInfo {
                module: submodule,
                line,
                is_relative,
                relative_level,
            });
        }
    } else if !imported_names.is_empty() && is_relative {
        // For "from . import X", the imported name IS the module
        for name in &imported_names {
            let resolved = resolve_relative_import(root, current_file, relative_level, Some(name));
            if !resolved.is_empty() {
                imports.push(ImportInfo {
                    module: resolved,
                    line,
                    is_relative,
                    relative_level,
                });
            }
        }
    }
}

/// Resolve a relative import to an absolute module path
fn resolve_relative_import(
    root: &Path,
    current_file: &Path,
    level: u32,
    module_name: Option<&str>,
) -> String {
    // Get the current file's directory (this is the current package)
    let current_dir = current_file.parent().unwrap_or(current_file);

    // Navigate up (level - 1) directories
    // In Python:
    //   . (level=1) = current package (don't go up)
    //   .. (level=2) = parent package (go up 1 dir)
    //   ... (level=3) = grandparent package (go up 2 dirs)
    let mut target_dir = current_dir;
    for _ in 1..level {
        target_dir = target_dir.parent().unwrap_or(target_dir);
    }

    // Convert to module path
    let base_module = match target_dir.strip_prefix(root) {
        Ok(relative) => relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("."),
        Err(_) => String::new(),
    };

    // Append the module name if present
    match (base_module.is_empty(), module_name) {
        (true, Some(name)) => name.to_string(),
        (false, Some(name)) => format!("{}.{}", base_module, name),
        (false, None) => base_module,
        (true, None) => String::new(),
    }
}

/// Check if a module path matches an import
fn module_matches_import(module: &ModulePath, import: &ImportInfo, _root: &Path) -> bool {
    let module_str = module.as_str();
    let import_module = &import.module;

    // Direct match - importing exactly this module
    if module_str == import_module {
        return true;
    }

    // Check if import references a submodule of this module
    // e.g., module="pkg" matches import="pkg.submodule"
    // This creates a dependency because importing pkg.submodule requires loading pkg
    if import_module.starts_with(&format!("{}.", module_str)) {
        return true;
    }

    // NOTE: We removed the check for module.starts_with(import) because:
    // If you import "pkg", you depend on "pkg", NOT on "pkg.module_a"
    // That check was causing false positives in cycle detection

    false
}

/// Find cycles in the dependency graph using Tarjan's algorithm
fn find_cycles(
    graph: &HashMap<ModulePath, DependencyNode>,
    module_to_file: &HashMap<ModulePath, String>,
    root: &Path,
) -> Vec<Vec<ModulePath>> {
    let mut index_counter = 0u32;
    let mut stack: Vec<ModulePath> = Vec::new();
    let mut lowlinks: HashMap<ModulePath, u32> = HashMap::new();
    let mut index: HashMap<ModulePath, u32> = HashMap::new();
    let mut on_stack: HashSet<ModulePath> = HashSet::new();
    let mut sccs: Vec<Vec<ModulePath>> = Vec::new();

    // Build adjacency list for efficient lookup
    let adjacency = build_adjacency_list(graph, module_to_file, root);

    for module in graph.keys() {
        if !index.contains_key(module) {
            tarjan_strongconnect(
                module.clone(),
                &adjacency,
                &mut index_counter,
                &mut stack,
                &mut lowlinks,
                &mut index,
                &mut on_stack,
                &mut sccs,
            );
        }
    }

    // Filter to only return SCCs with more than one node (actual cycles)
    sccs.into_iter().filter(|scc| scc.len() > 1).collect()
}

/// Build an adjacency list from the dependency graph
fn build_adjacency_list(
    graph: &HashMap<ModulePath, DependencyNode>,
    module_to_file: &HashMap<ModulePath, String>,
    root: &Path,
) -> HashMap<ModulePath, Vec<ModulePath>> {
    let mut adjacency: HashMap<ModulePath, Vec<ModulePath>> = HashMap::new();

    for (module, node) in graph {
        let mut edges = Vec::new();

        for import in &node.imports {
            // Find the best matching module for this import
            // Prefer exact matches over prefix matches
            let mut best_match: Option<&ModulePath> = None;
            let mut best_is_exact = false;

            for target_module in module_to_file.keys() {
                let target_str = target_module.as_str();
                let import_str = &import.module;

                // Check for exact match
                if target_str == import_str {
                    best_match = Some(target_module);
                    best_is_exact = true;
                    break; // Exact match is the best possible
                }

                // Check for prefix match (importing a submodule depends on the parent)
                if !best_is_exact && import_str.starts_with(&format!("{}.", target_str)) {
                    best_match = Some(target_module);
                    // Don't break - keep looking for an exact match
                }
            }

            if let Some(target) = best_match {
                edges.push(target.clone());
            }
        }

        adjacency.insert(module.clone(), edges);
    }

    adjacency
}

/// Tarjan's strongly connected components algorithm
fn tarjan_strongconnect(
    module: ModulePath,
    adjacency: &HashMap<ModulePath, Vec<ModulePath>>,
    index_counter: &mut u32,
    stack: &mut Vec<ModulePath>,
    lowlinks: &mut HashMap<ModulePath, u32>,
    index: &mut HashMap<ModulePath, u32>,
    on_stack: &mut HashSet<ModulePath>,
    sccs: &mut Vec<Vec<ModulePath>>,
) {
    // Set the depth index for this node
    index.insert(module.clone(), *index_counter);
    lowlinks.insert(module.clone(), *index_counter);
    *index_counter += 1;
    stack.push(module.clone());
    on_stack.insert(module.clone());

    // Consider successors
    if let Some(neighbors) = adjacency.get(&module) {
        for neighbor in neighbors {
            if !index.contains_key(neighbor) {
                // Successor has not yet been visited; recurse on it
                tarjan_strongconnect(
                    neighbor.clone(),
                    adjacency,
                    index_counter,
                    stack,
                    lowlinks,
                    index,
                    on_stack,
                    sccs,
                );
                let neighbor_lowlink = *lowlinks.get(neighbor).unwrap_or(&0);
                let module_lowlink = *lowlinks.get(&module).unwrap_or(&0);
                lowlinks.insert(module.clone(), module_lowlink.min(neighbor_lowlink));
            } else if on_stack.contains(neighbor) {
                // Successor is in stack and hence in the current SCC
                let neighbor_index = *index.get(neighbor).unwrap_or(&0);
                let module_lowlink = *lowlinks.get(&module).unwrap_or(&0);
                lowlinks.insert(module.clone(), module_lowlink.min(neighbor_index));
            }
        }
    }

    // If module is a root node, pop the stack and generate an SCC
    let module_lowlink = *lowlinks.get(&module).unwrap_or(&0);
    let module_index = *index.get(&module).unwrap_or(&0);

    if module_lowlink == module_index {
        let mut scc = Vec::new();
        loop {
            if let Some(w) = stack.pop() {
                on_stack.remove(&w);
                scc.push(w.clone());
                if w == module {
                    break;
                }
            } else {
                break;
            }
        }
        if !scc.is_empty() {
            sccs.push(scc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, relative_path: &str, content: &str) {
        let file_path = dir.join(relative_path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(file_path, content).unwrap();
    }

    #[test]
    fn test_no_circular_deps() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "module_a.py", "import module_b\n");
        create_test_file(root, "module_b.py", "import module_c\n");
        create_test_file(root, "module_c.py", "# no imports\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_simple_circular_dep() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "module_a.py", "import module_b\n");
        create_test_file(root, "module_b.py", "import module_a\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 1);
        assert!(result.structured_output.issues[0]
            .message
            .contains("Circular dependency detected"));
    }

    #[test]
    fn test_three_way_circular_dep() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "module_a.py", "import module_b\n");
        create_test_file(root, "module_b.py", "import module_c\n");
        create_test_file(root, "module_c.py", "import module_a\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 1);
    }

    #[test]
    fn test_from_import() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "module_a.py", "from module_b import something\n");
        create_test_file(root, "module_b.py", "from module_a import other\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 1);
    }

    #[test]
    fn test_package_import() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "pkg/__init__.py", "");
        create_test_file(root, "pkg/module_a.py", "from pkg import module_b\n");
        create_test_file(root, "pkg/module_b.py", "from pkg import module_a\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 1);
    }

    #[test]
    fn test_relative_import() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "pkg/__init__.py", "");
        create_test_file(root, "pkg/module_a.py", "from . import module_b\n");
        create_test_file(root, "pkg/module_b.py", "from . import module_a\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 1);
    }

    #[test]
    fn test_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let result = analyze(temp_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 0);
    }

    #[test]
    fn test_nonexistent_directory() {
        let result = analyze("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }

    #[test]
    fn test_file_path_to_module() {
        let root = Path::new("/project");

        // Regular file
        let result = file_path_to_module(root, Path::new("/project/foo/bar.py"));
        assert_eq!(result.as_str(), "foo.bar");

        // __init__.py
        let result = file_path_to_module(root, Path::new("/project/foo/__init__.py"));
        assert_eq!(result.as_str(), "foo");

        // Root level file
        let result = file_path_to_module(root, Path::new("/project/main.py"));
        assert_eq!(result.as_str(), "main");
    }

    #[test]
    fn test_multiple_independent_cycles() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Cycle 1: a <-> b
        create_test_file(root, "module_a.py", "import module_b\n");
        create_test_file(root, "module_b.py", "import module_a\n");

        // Cycle 2: c <-> d
        create_test_file(root, "module_c.py", "import module_d\n");
        create_test_file(root, "module_d.py", "import module_c\n");

        let result = analyze(root.to_str().unwrap()).unwrap();
        assert_eq!(result.issues_found, 2);
    }
}
