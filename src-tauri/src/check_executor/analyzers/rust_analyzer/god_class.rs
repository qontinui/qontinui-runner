//! God Class Detector for Rust
//!
//! Detects Rust structs with impl blocks that are too large (too many lines or methods),
//! indicating a potential design issue where a type has too many responsibilities.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use syn::{ImplItem, Item};

/// Default thresholds for god class detection
const DEFAULT_MAX_LINES: u32 = 500;
const DEFAULT_MAX_METHODS: u32 = 30;

/// Information about a detected impl block (or aggregated by type)
#[derive(Debug, Clone)]
struct ImplInfo {
    type_name: String,
    start_line: u32,
    end_line: u32,
    lines: u32,
    method_count: u32,
    associated_fn_count: u32,
    const_count: u32,
}

/// Aggregated information about a type across all its impl blocks
#[derive(Debug, Default)]
struct TypeInfo {
    type_name: String,
    first_line: u32,
    last_line: u32,
    total_lines: u32,
    method_count: u32,
    associated_fn_count: u32,
    const_count: u32,
    impl_count: u32,
}

impl TypeInfo {
    fn new(name: String, line: u32) -> Self {
        Self {
            type_name: name,
            first_line: line,
            last_line: line,
            ..Default::default()
        }
    }

    fn merge_impl(&mut self, impl_info: &ImplInfo) {
        self.first_line = self.first_line.min(impl_info.start_line);
        self.last_line = self.last_line.max(impl_info.end_line);
        self.total_lines += impl_info.lines;
        self.method_count += impl_info.method_count;
        self.associated_fn_count += impl_info.associated_fn_count;
        self.const_count += impl_info.const_count;
        self.impl_count += 1;
    }

    fn total_methods(&self) -> u32 {
        self.method_count + self.associated_fn_count
    }
}

/// Analyze Rust files for god classes with default thresholds
pub fn analyze_with_defaults(working_dir: &str) -> Result<ParsedOutput, String> {
    analyze(working_dir, DEFAULT_MAX_LINES, DEFAULT_MAX_METHODS)
}

/// Analyze Rust files for god classes with custom thresholds
pub fn analyze(
    working_dir: &str,
    max_lines: u32,
    max_methods: u32,
) -> Result<ParsedOutput, String> {
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
    let mut files_with_issues = std::collections::HashSet::new();
    let mut total_types = 0u32;
    let mut files_checked = 0u32;

    for file_path in files {
        files_checked += 1;

        match analyze_file(&file_path, max_lines, max_methods) {
            Ok((type_issues, type_count)) => {
                total_types += type_count;
                for (issue, file_str) in type_issues {
                    files_with_issues.insert(file_str);
                    issues.push(issue);
                }
            }
            Err(e) => {
                tracing::warn!("Error analyzing {}: {}", file_path.display(), e);
            }
        }
    }

    let issues_found = issues.len() as u32;

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
                    ("warning".to_string(), issues_found),
                    ("types_analyzed".to_string(), total_types),
                ]),
            }),
            tool_data: HashMap::from([
                (
                    "max_lines_threshold".to_string(),
                    serde_json::json!(max_lines),
                ),
                (
                    "max_methods_threshold".to_string(),
                    serde_json::json!(max_methods),
                ),
                (
                    "total_types_analyzed".to_string(),
                    serde_json::json!(total_types),
                ),
            ]),
        },
    })
}

/// Analyze a single Rust file for god classes
fn analyze_file(
    path: &Path,
    max_lines: u32,
    max_methods: u32,
) -> Result<(Vec<(crate::check_executor::types::CheckIssue, String)>, u32), String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

    let syntax = syn::parse_file(&content)
        .map_err(|e| format!("Failed to parse file {}: {}", path.display(), e))?;

    let file_str = path.to_string_lossy().to_string();

    // Collect impl blocks by type name
    let mut type_map: HashMap<String, TypeInfo> = HashMap::new();

    for item in &syntax.items {
        if let Item::Impl(impl_block) = item {
            // Skip trait impls for this analysis (focus on inherent impls)
            if impl_block.trait_.is_some() {
                continue;
            }

            let type_name = extract_type_name(&impl_block.self_ty);
            let impl_info = analyze_impl_block(impl_block);

            type_map
                .entry(type_name.clone())
                .or_insert_with(|| TypeInfo::new(type_name, impl_info.start_line))
                .merge_impl(&impl_info);
        }
    }

    let type_count = type_map.len() as u32;
    let mut issues = Vec::new();

    // Check each type against thresholds
    for (_type_name, type_info) in type_map {
        let total_methods = type_info.total_methods();

        if type_info.total_lines > max_lines || total_methods > max_methods {
            let message = format!(
                "God struct detected: {} has {} impl lines and {} methods across {} impl block(s) (thresholds: {} lines, {} methods)",
                type_info.type_name,
                type_info.total_lines,
                total_methods,
                type_info.impl_count,
                max_lines,
                max_methods
            );

            let issue = IssueBuilder::new(&file_str, message)
                .line(type_info.first_line)
                .end_line(type_info.last_line)
                .code("GOD_CLASS_RUST")
                .warning()
                .build();

            issues.push((issue, file_str.clone()));
        }
    }

    Ok((issues, type_count))
}

/// Analyze an impl block and return its information
fn analyze_impl_block(impl_block: &syn::ItemImpl) -> ImplInfo {
    let type_name = extract_type_name(&impl_block.self_ty);

    // Use proc_macro2 span to get line numbers
    let start_line = impl_block.impl_token.span.start().line as u32;
    let end_line = impl_block.brace_token.span.close().end().line as u32;
    let lines = end_line.saturating_sub(start_line) + 1;

    let mut method_count = 0u32;
    let mut associated_fn_count = 0u32;
    let mut const_count = 0u32;

    for item in &impl_block.items {
        match item {
            ImplItem::Fn(method) => {
                // Check if it's a method (has self parameter) or associated function
                let has_receiver = method
                    .sig
                    .inputs
                    .iter()
                    .any(|arg| matches!(arg, syn::FnArg::Receiver(_)));

                if has_receiver {
                    method_count += 1;
                } else {
                    associated_fn_count += 1;
                }
            }
            ImplItem::Const(_) => {
                const_count += 1;
            }
            _ => {}
        }
    }

    ImplInfo {
        type_name,
        start_line,
        end_line,
        lines,
        method_count,
        associated_fn_count,
        const_count,
    }
}

/// Extract the type name from a syn::Type
fn extract_type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        syn::Type::Reference(type_ref) => extract_type_name(&type_ref.elem),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_analyze_small_struct() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
struct SmallStruct {
    value: i32,
}

impl SmallStruct {
    fn new(value: i32) -> Self {
        Self { value }
    }

    fn get_value(&self) -> i32 {
        self.value
    }

    fn set_value(&mut self, v: i32) {
        self.value = v;
    }
}
"#;
        create_test_file(temp_dir.path(), "small.rs", code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Small struct should not be flagged
        assert_eq!(result.issues_found, 0);
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn test_analyze_god_struct_by_methods() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a struct with many methods
        let mut code = String::from("struct ManyMethods {}\n\nimpl ManyMethods {\n");
        for i in 0..35 {
            code.push_str(&format!("    fn method_{}(&self) {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "many_methods.rs", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Should be flagged for too many methods
        assert_eq!(result.issues_found, 1);
        assert!(result.structured_output.issues[0]
            .message
            .contains("ManyMethods"));
        assert!(result.structured_output.issues[0]
            .message
            .contains("35 methods"));
    }

    #[test]
    fn test_analyze_god_struct_by_lines() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a large impl with many lines but few methods
        let mut code = String::from("struct LargeStruct {}\n\nimpl LargeStruct {\n");
        code.push_str("    fn large_method(&self) {\n");
        // Add enough lines to exceed 100 lines (using lower threshold for testing)
        for i in 0..120 {
            code.push_str(&format!("        let x{} = {};\n", i, i));
        }
        code.push_str("    }\n");
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "large.rs", &code);

        // Use lower threshold for testing
        let result = analyze(temp_dir.path().to_str().unwrap(), 100, 30).unwrap();

        // Should be flagged for too many lines
        assert_eq!(result.issues_found, 1);
        assert!(result.structured_output.issues[0]
            .message
            .contains("LargeStruct"));
    }

    #[test]
    fn test_analyze_multiple_impl_blocks() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a struct with multiple impl blocks
        let mut code = String::from("struct MultiImpl {}\n\n");

        // First impl block with 20 methods
        code.push_str("impl MultiImpl {\n");
        for i in 0..20 {
            code.push_str(&format!("    fn method_a{}(&self) {{}}\n", i));
        }
        code.push_str("}\n\n");

        // Second impl block with 15 methods
        code.push_str("impl MultiImpl {\n");
        for i in 0..15 {
            code.push_str(&format!("    fn method_b{}(&self) {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "multi_impl.rs", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Should be flagged for combined methods (20 + 15 = 35 > 30)
        assert_eq!(result.issues_found, 1);
        assert!(result.structured_output.issues[0]
            .message
            .contains("MultiImpl"));
        assert!(result.structured_output.issues[0]
            .message
            .contains("2 impl block"));
    }

    #[test]
    fn test_analyze_with_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let code = r#"
struct NormalStruct {}

impl NormalStruct {
    fn method(&self) {}
}
"#;
        create_test_file(temp_dir.path(), "normal.rs", code);

        let result = analyze_with_defaults(temp_dir.path().to_str().unwrap()).unwrap();

        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_analyze_nonexistent_directory() {
        let result = analyze("/nonexistent/path/that/does/not/exist", 500, 30);
        assert!(result.is_err());
    }

    #[test]
    fn test_ignores_trait_impls() {
        let temp_dir = TempDir::new().unwrap();

        // Create a struct with a large trait impl (should be ignored)
        let mut code = String::from("struct MyStruct {}\n\n");
        code.push_str("trait MyTrait {\n");
        for i in 0..35 {
            code.push_str(&format!("    fn trait_method_{}(&self);\n", i));
        }
        code.push_str("}\n\n");
        code.push_str("impl MyTrait for MyStruct {\n");
        for i in 0..35 {
            code.push_str(&format!("    fn trait_method_{}(&self) {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "trait_impl.rs", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Trait impls are ignored, so no issues
        assert_eq!(result.issues_found, 0);
    }

    #[test]
    fn test_counts_associated_functions() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a struct with many associated functions (no self)
        let mut code = String::from("struct WithAssociated {}\n\nimpl WithAssociated {\n");
        for i in 0..35 {
            code.push_str(&format!("    fn assoc_fn_{}() {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "associated.rs", &code);

        let result = analyze(temp_dir.path().to_str().unwrap(), 500, 30).unwrap();

        // Should be flagged for too many methods (associated functions count too)
        assert_eq!(result.issues_found, 1);
    }

    #[test]
    fn test_issue_message_format() {
        let temp_dir = TempDir::new().unwrap();

        // Generate a struct that exceeds thresholds
        let mut code = String::from("struct GodStruct {}\n\nimpl GodStruct {\n");
        for i in 0..5 {
            code.push_str(&format!("    fn method_{}(&self) {{}}\n", i));
        }
        code.push_str("}\n");

        create_test_file(temp_dir.path(), "god.rs", &code);

        // Use very low thresholds to trigger
        let result = analyze(temp_dir.path().to_str().unwrap(), 5, 3).unwrap();

        assert_eq!(result.issues_found, 1);
        let issue = &result.structured_output.issues[0];

        // Check message format
        assert!(issue.message.contains("God struct detected:"));
        assert!(issue.message.contains("GodStruct"));
        assert!(issue.message.contains("(thresholds: 5 lines, 3 methods)"));

        // Check issue metadata
        assert_eq!(issue.code, Some("GOD_CLASS_RUST".to_string()));
        assert!(issue.line.is_some());
    }
}
