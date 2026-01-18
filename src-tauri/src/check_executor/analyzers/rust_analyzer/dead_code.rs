//! Rust Dead Code Analyzer
//!
//! Finds potentially unused code in Rust projects:
//! - Unused imports
//! - Unused functions (not called or exported)
//!
//! Note: This is a simplified heuristic-based analyzer. Full dead code detection
//! requires full compilation with the Rust compiler. This analyzer catches
//! common cases but may have false positives/negatives.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{Expr, Pat, UseTree};

/// Information about an import
#[derive(Debug, Clone)]
struct ImportInfo {
    name: String,
    line: u32,
    is_glob: bool,
    is_prelude: bool,
}

/// Information about a function definition
#[derive(Debug, Clone)]
struct FunctionInfo {
    name: String,
    line: u32,
    is_public: bool,
    is_main: bool,
    is_test: bool,
    has_test_attribute: bool,
    is_exported: bool,
}

/// Visitor that collects imports and their usages
struct ImportVisitor {
    imports: Vec<ImportInfo>,
    used_names: HashSet<String>,
    current_depth: usize,
}

impl ImportVisitor {
    fn new() -> Self {
        Self {
            imports: Vec::new(),
            used_names: HashSet::new(),
            current_depth: 0,
        }
    }

    fn collect_use_tree(&mut self, tree: &UseTree, prefix: &str, line: u32) {
        match tree {
            UseTree::Path(path) => {
                let new_prefix = if prefix.is_empty() {
                    path.ident.to_string()
                } else {
                    format!("{}::{}", prefix, path.ident)
                };
                self.collect_use_tree(&path.tree, &new_prefix, line);
            }
            UseTree::Name(name) => {
                let import_name = name.ident.to_string();
                let is_prelude = prefix.contains("prelude") || prefix.contains("std::prelude");
                self.imports.push(ImportInfo {
                    name: import_name,
                    line,
                    is_glob: false,
                    is_prelude,
                });
            }
            UseTree::Rename(rename) => {
                // use foo as bar - track 'bar' as the used name
                let import_name = rename.rename.to_string();
                self.imports.push(ImportInfo {
                    name: import_name,
                    line,
                    is_glob: false,
                    is_prelude: false,
                });
            }
            UseTree::Glob(_) => {
                // Glob imports are hard to track, mark as potentially used
                self.imports.push(ImportInfo {
                    name: format!("{}::*", prefix),
                    line,
                    is_glob: true,
                    is_prelude: prefix.contains("prelude"),
                });
            }
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.collect_use_tree(tree, prefix, line);
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for ImportVisitor {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        let line = node.use_token.span.start().line as u32;
        self.collect_use_tree(&node.tree, "", line);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        // Track first segment as a used name
        if let Some(first) = path.segments.first() {
            self.used_names.insert(first.ident.to_string());
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.used_names.insert(ident.to_string());
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        // Track macro invocations which might use imports
        if let Expr::Macro(mac) = expr {
            if let Some(first) = mac.mac.path.segments.first() {
                self.used_names.insert(first.ident.to_string());
            }
        }
        syn::visit::visit_expr(self, expr);
    }

    fn visit_type(&mut self, ty: &'ast syn::Type) {
        // Track types used
        if let syn::Type::Path(type_path) = ty {
            if let Some(first) = type_path.path.segments.first() {
                self.used_names.insert(first.ident.to_string());
            }
        }
        syn::visit::visit_type(self, ty);
    }

    fn visit_pat(&mut self, pat: &'ast Pat) {
        // Track pattern bindings and struct patterns
        if let Pat::Struct(struct_pat) = pat {
            if let Some(first) = struct_pat.path.segments.first() {
                self.used_names.insert(first.ident.to_string());
            }
        }
        syn::visit::visit_pat(self, pat);
    }
}

/// Visitor that collects function definitions and usages
struct FunctionVisitor {
    functions: Vec<FunctionInfo>,
    called_functions: HashSet<String>,
    in_test_module: bool,
}

impl FunctionVisitor {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            called_functions: HashSet::new(),
            in_test_module: false,
        }
    }

    fn has_attribute(attrs: &[syn::Attribute], name: &str) -> bool {
        attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .map(|s| s.ident == name)
                .unwrap_or(false)
        })
    }

    fn is_public_fn(vis: &syn::Visibility) -> bool {
        !matches!(vis, syn::Visibility::Inherited)
    }
}

impl<'ast> Visit<'ast> for FunctionVisitor {
    fn visit_item_fn(&mut self, func: &'ast syn::ItemFn) {
        let name = func.sig.ident.to_string();
        let has_test_attr = Self::has_attribute(&func.attrs, "test");
        let has_bench_attr = Self::has_attribute(&func.attrs, "bench");
        let has_tokio_test = func.attrs.iter().any(|attr| {
            attr.path()
                .segments
                .iter()
                .any(|s| s.ident == "tokio" || s.ident == "test")
        });

        self.functions.push(FunctionInfo {
            name: name.clone(),
            line: func.sig.ident.span().start().line as u32,
            is_public: Self::is_public_fn(&func.vis),
            is_main: name == "main",
            is_test: has_test_attr || has_bench_attr || has_tokio_test || self.in_test_module,
            has_test_attribute: has_test_attr,
            is_exported: Self::is_public_fn(&func.vis),
        });

        syn::visit::visit_item_fn(self, func);
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        // Check if this is a test module
        let was_in_test = self.in_test_module;
        let is_test_mod = module.ident == "tests"
            || module.ident == "test"
            || Self::has_attribute(&module.attrs, "cfg");

        if is_test_mod {
            self.in_test_module = true;
        }

        syn::visit::visit_item_mod(self, module);

        self.in_test_module = was_in_test;
    }

    fn visit_impl_item_fn(&mut self, method: &'ast syn::ImplItemFn) {
        let name = method.sig.ident.to_string();
        let has_test_attr = Self::has_attribute(&method.attrs, "test");

        self.functions.push(FunctionInfo {
            name,
            line: method.sig.ident.span().start().line as u32,
            is_public: Self::is_public_fn(&method.vis),
            is_main: false,
            is_test: has_test_attr || self.in_test_module,
            has_test_attribute: has_test_attr,
            is_exported: Self::is_public_fn(&method.vis),
        });

        syn::visit::visit_impl_item_fn(self, method);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // Track function calls
        if let Expr::Path(path) = call.func.as_ref() {
            if let Some(last) = path.path.segments.last() {
                self.called_functions.insert(last.ident.to_string());
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        // Track method calls
        self.called_functions.insert(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }
}

/// Analyze a single Rust file for unused imports
fn analyze_file_imports(path: &Path) -> Result<Vec<ImportInfo>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

    let syntax = syn::parse_file(&content)
        .map_err(|e| format!("Failed to parse file {}: {}", path.display(), e))?;

    let mut visitor = ImportVisitor::new();
    visitor.visit_file(&syntax);

    // Find unused imports
    let unused: Vec<ImportInfo> = visitor
        .imports
        .into_iter()
        .filter(|import| {
            // Skip glob imports and prelude imports
            if import.is_glob || import.is_prelude {
                return false;
            }
            // Check if the import name is used
            !visitor.used_names.contains(&import.name)
        })
        .collect();

    Ok(unused)
}

/// Analyze a project for potentially unused functions
fn analyze_project_functions(
    root: &Path,
    config: &WalkConfig,
) -> Result<Vec<(String, FunctionInfo)>, String> {
    let files = walk_files(root, config);

    // First pass: collect all function definitions and calls
    let mut all_functions: Vec<(String, FunctionInfo)> = Vec::new();
    let mut all_called: HashSet<String> = HashSet::new();

    for file_path in &files {
        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let syntax = match syn::parse_file(&content) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut visitor = FunctionVisitor::new();
        visitor.visit_file(&syntax);

        let file_str = file_path.to_string_lossy().to_string();
        for func in visitor.functions {
            all_functions.push((file_str.clone(), func));
        }
        all_called.extend(visitor.called_functions);
    }

    // Find unused functions
    let unused: Vec<(String, FunctionInfo)> = all_functions
        .into_iter()
        .filter(|(_, func)| {
            // Skip main, tests, and public functions (might be API)
            if func.is_main || func.is_test || func.has_test_attribute {
                return false;
            }

            // Public functions in lib.rs or mod.rs might be API
            if func.is_exported {
                return false;
            }

            // Check if function is called
            !all_called.contains(&func.name)
        })
        .collect();

    Ok(unused)
}

/// Analyze all Rust files in a directory for dead code
pub fn analyze(working_dir: &str) -> Result<ParsedOutput, String> {
    let root = Path::new(working_dir);
    if !root.exists() {
        return Err(format!("Directory does not exist: {}", working_dir));
    }

    let config = WalkConfig {
        extensions: vec!["rs".to_string()],
        ..WalkConfig::default()
    };

    let files = walk_files(root, &config);
    let mut issues = Vec::new();
    let mut files_checked = 0u32;
    let mut files_with_issues = HashSet::new();
    let mut unused_imports_count = 0u32;
    let mut unused_functions_count = 0u32;

    // Analyze imports per file
    for file_path in &files {
        files_checked += 1;

        match analyze_file_imports(file_path) {
            Ok(unused_imports) => {
                if !unused_imports.is_empty() {
                    files_with_issues.insert(file_path.to_string_lossy().to_string());
                }

                for import in unused_imports {
                    unused_imports_count += 1;
                    issues.push(
                        IssueBuilder::new(
                            file_path.to_string_lossy().to_string(),
                            format!("Unused import: '{}'", import.name),
                        )
                        .line(import.line)
                        .code("RUST_UNUSED_IMPORT")
                        .warning()
                        .fixable(true)
                        .build(),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Error analyzing imports in {}: {}", file_path.display(), e);
            }
        }
    }

    // Analyze functions project-wide
    match analyze_project_functions(root, &config) {
        Ok(unused_funcs) => {
            for (file, func) in unused_funcs {
                unused_functions_count += 1;
                files_with_issues.insert(file.clone());

                issues.push(
                    IssueBuilder::new(
                        file,
                        format!(
                            "Potentially unused function: '{}' (not called within project)",
                            func.name
                        ),
                    )
                    .line(func.line)
                    .code("RUST_UNUSED_FUNCTION")
                    .info() // Info level since this is heuristic-based
                    .build(),
                );
            }
        }
        Err(e) => {
            tracing::warn!("Error analyzing functions: {}", e);
        }
    }

    let issues_found = issues.len() as u32;
    let warning_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Warning)
        .count() as u32;
    let info_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Info)
        .count() as u32;

    Ok(ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: files_checked,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("warning".to_string(), warning_count),
                    ("info".to_string(), info_count),
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "unused_imports".to_string(),
                    serde_json::json!(unused_imports_count),
                ),
                (
                    "unused_functions".to_string(),
                    serde_json::json!(unused_functions_count),
                ),
            ]),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_analyze_imports(code: &str) -> (Vec<ImportInfo>, HashSet<String>) {
        let syntax = syn::parse_file(code).unwrap();
        let mut visitor = ImportVisitor::new();
        visitor.visit_file(&syntax);
        (visitor.imports, visitor.used_names)
    }

    #[test]
    fn test_used_import() {
        let code = r#"
            use std::collections::HashMap;

            fn main() {
                let map: HashMap<String, i32> = HashMap::new();
            }
        "#;

        let (imports, used) = parse_and_analyze_imports(code);
        assert!(!imports.is_empty(), "Should find imports");
        assert!(used.contains("HashMap"), "Should track HashMap as used");
    }

    #[test]
    fn test_unused_import() {
        let code = r#"
            use std::collections::HashMap;
            use std::collections::HashSet;

            fn main() {
                let map: HashMap<String, i32> = HashMap::new();
            }
        "#;

        let (imports, used) = parse_and_analyze_imports(code);
        assert_eq!(imports.len(), 2, "Should find 2 imports");
        assert!(used.contains("HashMap"), "HashMap should be used");
        assert!(
            !used.contains("HashSet"),
            "HashSet should not be used"
        );
    }

    #[test]
    fn test_renamed_import() {
        let code = r#"
            use std::collections::HashMap as Map;

            fn main() {
                let m: Map<String, i32> = Map::new();
            }
        "#;

        let (imports, used) = parse_and_analyze_imports(code);
        assert!(
            imports.iter().any(|i| i.name == "Map"),
            "Should track renamed import as 'Map'"
        );
        assert!(used.contains("Map"), "Should track Map as used");
    }

    #[test]
    fn test_glob_import() {
        let code = r#"
            use std::collections::*;
        "#;

        let (imports, _) = parse_and_analyze_imports(code);
        assert!(
            imports.iter().any(|i| i.is_glob),
            "Should detect glob import"
        );
    }

    fn parse_and_analyze_functions(code: &str) -> (Vec<FunctionInfo>, HashSet<String>) {
        let syntax = syn::parse_file(code).unwrap();
        let mut visitor = FunctionVisitor::new();
        visitor.visit_file(&syntax);
        (visitor.functions, visitor.called_functions)
    }

    #[test]
    fn test_main_function() {
        let code = r#"
            fn main() {}
        "#;

        let (funcs, _) = parse_and_analyze_functions(code);
        assert!(funcs.iter().any(|f| f.is_main), "Should detect main function");
    }

    #[test]
    fn test_test_function() {
        let code = r#"
            #[test]
            fn test_something() {}
        "#;

        let (funcs, _) = parse_and_analyze_functions(code);
        assert!(
            funcs.iter().any(|f| f.has_test_attribute),
            "Should detect test function"
        );
    }

    #[test]
    fn test_function_call_tracking() {
        let code = r#"
            fn helper() {}

            fn main() {
                helper();
            }
        "#;

        let (_, called) = parse_and_analyze_functions(code);
        assert!(called.contains("helper"), "Should track helper as called");
    }

    #[test]
    fn test_public_function() {
        let code = r#"
            pub fn public_api() {}
            fn private_fn() {}
        "#;

        let (funcs, _) = parse_and_analyze_functions(code);
        let public_fn = funcs.iter().find(|f| f.name == "public_api").unwrap();
        let private_fn = funcs.iter().find(|f| f.name == "private_fn").unwrap();

        assert!(public_fn.is_public, "public_api should be public");
        assert!(!private_fn.is_public, "private_fn should not be public");
    }
}
