//! Rust Cyclomatic Complexity Analyzer
//!
//! Analyzes Rust source files to calculate cyclomatic complexity for each function.
//! Uses the `syn` crate to parse Rust code and counts decision points.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{Expr, Item, Pat};

/// Default maximum complexity threshold
const DEFAULT_MAX_COMPLEXITY: u32 = 15;

/// Information about a function's complexity
#[derive(Debug, Clone)]
struct FunctionComplexity {
    name: String,
    line: u32,
    complexity: u32,
}

/// Visitor that calculates cyclomatic complexity for functions
struct ComplexityVisitor {
    /// Current complexity count for the function being analyzed
    current_complexity: u32,
    /// Whether we're inside a function
    in_function: bool,
}

impl ComplexityVisitor {
    fn new() -> Self {
        Self {
            current_complexity: 1, // Base complexity
            in_function: false,
        }
    }

    fn reset(&mut self) {
        self.current_complexity = 1;
    }
}

impl<'ast> Visit<'ast> for ComplexityVisitor {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            // Conditional expressions add to complexity
            Expr::If(_) => {
                self.current_complexity += 1;
            }
            // Match arms (each arm is a decision point, minus one for the base)
            Expr::Match(expr_match) => {
                // Each arm adds a decision point
                self.current_complexity += expr_match.arms.len().saturating_sub(1) as u32;
            }
            // Loop constructs
            Expr::While(_) | Expr::Loop(_) | Expr::ForLoop(_) => {
                self.current_complexity += 1;
            }
            // Short-circuit logical operators
            Expr::Binary(binary) => {
                use syn::BinOp;
                match binary.op {
                    BinOp::And(_) | BinOp::Or(_) => {
                        self.current_complexity += 1;
                    }
                    _ => {}
                }
            }
            // Question mark operator (early return)
            Expr::Try(_) => {
                self.current_complexity += 1;
            }
            // Return statements with conditions are handled by their parent if/match
            _ => {}
        }

        // Continue visiting nested expressions
        syn::visit::visit_expr(self, expr);
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        // Check for guard patterns in match arms
        if arm.guard.is_some() {
            self.current_complexity += 1;
        }
        syn::visit::visit_arm(self, arm);
    }

    fn visit_pat(&mut self, pat: &'ast Pat) {
        // Or patterns add complexity
        if let Pat::Or(_) = pat {
            self.current_complexity += 1;
        }
        syn::visit::visit_pat(self, pat);
    }
}

/// Analyze a single function item and return its complexity
fn analyze_function(func: &syn::ItemFn) -> FunctionComplexity {
    let mut visitor = ComplexityVisitor::new();
    visitor.in_function = true;

    // Visit the function body
    for stmt in &func.block.stmts {
        syn::visit::visit_stmt(&mut visitor, stmt);
    }

    FunctionComplexity {
        name: func.sig.ident.to_string(),
        line: func.sig.ident.span().start().line as u32,
        complexity: visitor.current_complexity,
    }
}

/// Analyze a method within an impl block
fn analyze_impl_method(method: &syn::ImplItemFn, impl_type: Option<&str>) -> FunctionComplexity {
    let mut visitor = ComplexityVisitor::new();
    visitor.in_function = true;

    // Visit the method body
    for stmt in &method.block.stmts {
        syn::visit::visit_stmt(&mut visitor, stmt);
    }

    let name = if let Some(type_name) = impl_type {
        format!("{}::{}", type_name, method.sig.ident)
    } else {
        method.sig.ident.to_string()
    };

    FunctionComplexity {
        name,
        line: method.sig.ident.span().start().line as u32,
        complexity: visitor.current_complexity,
    }
}

/// Analyze a trait method with a default implementation
fn analyze_trait_method(method: &syn::TraitItemFn, trait_name: &str) -> Option<FunctionComplexity> {
    // Only analyze methods with default implementations
    let block = method.default.as_ref()?;

    let mut visitor = ComplexityVisitor::new();
    visitor.in_function = true;

    for stmt in &block.stmts {
        syn::visit::visit_stmt(&mut visitor, stmt);
    }

    Some(FunctionComplexity {
        name: format!("{}::{}", trait_name, method.sig.ident),
        line: method.sig.ident.span().start().line as u32,
        complexity: visitor.current_complexity,
    })
}

/// Extract the type name from a syn::Type
fn extract_type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(type_path) => {
            type_path.path.segments.last().map(|seg| seg.ident.to_string())
        }
        _ => None,
    }
}

/// Analyze a single Rust file
fn analyze_file(path: &Path) -> Result<Vec<FunctionComplexity>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

    let syntax = syn::parse_file(&content)
        .map_err(|e| format!("Failed to parse file {}: {}", path.display(), e))?;

    let mut functions = Vec::new();

    for item in syntax.items {
        match item {
            // Standalone functions
            Item::Fn(func) => {
                functions.push(analyze_function(&func));
            }
            // Impl blocks
            Item::Impl(impl_block) => {
                let type_name = extract_type_name(&impl_block.self_ty);
                for impl_item in impl_block.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        functions.push(analyze_impl_method(&method, type_name.as_deref()));
                    }
                }
            }
            // Trait definitions with default methods
            Item::Trait(trait_def) => {
                let trait_name = trait_def.ident.to_string();
                for trait_item in trait_def.items {
                    if let syn::TraitItem::Fn(method) = trait_item {
                        if let Some(complexity) = analyze_trait_method(&method, &trait_name) {
                            functions.push(complexity);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(functions)
}

/// Analyze all Rust files in a directory for cyclomatic complexity
pub fn analyze(working_dir: &str, max_complexity: u32) -> Result<ParsedOutput, String> {
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
    let mut total_functions = 0u32;
    let mut complex_functions = 0u32;
    let mut total_complexity = 0u64;
    let mut files_checked = 0u32;
    let mut files_with_issues = std::collections::HashSet::new();

    for file_path in files {
        files_checked += 1;

        match analyze_file(&file_path) {
            Ok(functions) => {
                for func in functions {
                    total_functions += 1;
                    total_complexity += func.complexity as u64;

                    if func.complexity > max_complexity {
                        complex_functions += 1;
                        let file_str = file_path.to_string_lossy().to_string();
                        files_with_issues.insert(file_str.clone());

                        issues.push(
                            IssueBuilder::new(
                                file_str,
                                format!(
                                    "Function '{}' has cyclomatic complexity {} (threshold: {})",
                                    func.name, func.complexity, max_complexity
                                ),
                            )
                            .line(func.line)
                            .code("RUST_COMPLEXITY")
                            .severity(if func.complexity > max_complexity * 2 {
                                IssueSeverity::Error
                            } else {
                                IssueSeverity::Warning
                            })
                            .build(),
                        );
                    }
                }
            }
            Err(e) => {
                // Log parse errors but continue with other files
                tracing::warn!("Error analyzing {}: {}", file_path.display(), e);
            }
        }
    }

    let avg_complexity = if total_functions > 0 {
        total_complexity as f64 / total_functions as f64
    } else {
        0.0
    };

    let issues_found = issues.len() as u32;
    let error_count = issues
        .iter()
        .filter(|i| i.severity == IssueSeverity::Error)
        .count() as u32;
    let warning_count = issues_found - error_count;

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
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                ]),
            }),
            tool_data: HashMap::from([
                ("total_functions".to_string(), serde_json::json!(total_functions)),
                ("complex_functions".to_string(), serde_json::json!(complex_functions)),
                ("average_complexity".to_string(), serde_json::json!(avg_complexity)),
                ("max_threshold".to_string(), serde_json::json!(max_complexity)),
            ]),
        },
    })
}

/// Analyze with default threshold (15)
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MAX_COMPLEXITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_function_complexity() {
        let code = r#"
            fn simple() {
                let x = 1;
            }
        "#;

        let syntax: syn::ItemFn = syn::parse_str(code).unwrap();
        let result = analyze_function(&syntax);
        assert_eq!(result.complexity, 1, "Simple function should have complexity 1");
    }

    #[test]
    fn test_if_adds_complexity() {
        let code = r#"
            fn with_if() {
                if true {
                    let x = 1;
                }
            }
        "#;

        let syntax: syn::ItemFn = syn::parse_str(code).unwrap();
        let result = analyze_function(&syntax);
        assert_eq!(result.complexity, 2, "Function with if should have complexity 2");
    }

    #[test]
    fn test_nested_conditions() {
        let code = r#"
            fn nested() {
                if true {
                    if false {
                        let x = 1;
                    }
                }
            }
        "#;

        let syntax: syn::ItemFn = syn::parse_str(code).unwrap();
        let result = analyze_function(&syntax);
        assert_eq!(result.complexity, 3, "Nested if should have complexity 3");
    }

    #[test]
    fn test_match_complexity() {
        let code = r#"
            fn with_match() {
                match x {
                    1 => {},
                    2 => {},
                    3 => {},
                    _ => {},
                }
            }
        "#;

        let syntax: syn::ItemFn = syn::parse_str(code).unwrap();
        let result = analyze_function(&syntax);
        // 4 arms - 1 = 3, plus base = 4
        assert!(result.complexity >= 3, "Match with 4 arms should have complexity >= 3");
    }

    #[test]
    fn test_loops_add_complexity() {
        let code = r#"
            fn with_loops() {
                for i in 0..10 {
                    while true {
                        loop {
                            break;
                        }
                    }
                }
            }
        "#;

        let syntax: syn::ItemFn = syn::parse_str(code).unwrap();
        let result = analyze_function(&syntax);
        // Base 1 + for 1 + while 1 + loop 1 = 4
        assert_eq!(result.complexity, 4, "Function with 3 loops should have complexity 4");
    }

    #[test]
    fn test_logical_operators() {
        let code = r#"
            fn with_logical() {
                if a && b || c {
                    let x = 1;
                }
            }
        "#;

        let syntax: syn::ItemFn = syn::parse_str(code).unwrap();
        let result = analyze_function(&syntax);
        // Base 1 + if 1 + && 1 + || 1 = 4
        assert_eq!(result.complexity, 4, "Logical operators should add complexity");
    }
}
