//! Rust Unsafe Code Analyzer
//!
//! Finds and reports all `unsafe` blocks and `unsafe fn` declarations in Rust code.
//! Uses the `syn` crate to parse Rust code and identify unsafe constructs.

use crate::check_executor::analyzers::common::file_walker::{walk_files, WalkConfig};
use crate::check_executor::analyzers::common::issue_builder::IssueBuilder;
use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckStructuredOutput, CheckSummary, IssueSeverity};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, ImplItem, Item, TraitItem};

/// Types of unsafe constructs we can detect
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnsafeKind {
    /// `unsafe fn`
    UnsafeFunction,
    /// `unsafe` block
    UnsafeBlock,
    /// `unsafe impl`
    UnsafeImpl,
    /// `unsafe trait`
    UnsafeTrait,
    /// Raw pointer dereference (within unsafe block)
    RawPointerDeref,
    /// Calling an unsafe function (within unsafe block)
    UnsafeFnCall,
    /// Accessing/modifying mutable static
    MutableStaticAccess,
    /// Union field access
    UnionFieldAccess,
    /// FFI call (extern function)
    FfiCall,
}

impl UnsafeKind {
    fn as_str(&self) -> &'static str {
        match self {
            UnsafeKind::UnsafeFunction => "unsafe function",
            UnsafeKind::UnsafeBlock => "unsafe block",
            UnsafeKind::UnsafeImpl => "unsafe impl",
            UnsafeKind::UnsafeTrait => "unsafe trait",
            UnsafeKind::RawPointerDeref => "raw pointer dereference",
            UnsafeKind::UnsafeFnCall => "unsafe function call",
            UnsafeKind::MutableStaticAccess => "mutable static access",
            UnsafeKind::UnionFieldAccess => "union field access",
            UnsafeKind::FfiCall => "FFI call",
        }
    }
}

/// Information about an unsafe construct
#[derive(Debug, Clone)]
struct UnsafeLocation {
    kind: UnsafeKind,
    line: u32,
    name: Option<String>,
    reason: String,
}

/// Visitor that finds unsafe constructs
struct UnsafeVisitor {
    locations: Vec<UnsafeLocation>,
    current_function: Option<String>,
    in_unsafe_block: bool,
    extern_functions: std::collections::HashSet<String>,
}

impl UnsafeVisitor {
    fn new() -> Self {
        Self {
            locations: Vec::new(),
            current_function: None,
            in_unsafe_block: false,
            extern_functions: std::collections::HashSet::new(),
        }
    }

    fn add_unsafe(&mut self, kind: UnsafeKind, line: u32, name: Option<String>, reason: &str) {
        self.locations.push(UnsafeLocation {
            kind,
            line,
            name,
            reason: reason.to_string(),
        });
    }
}

impl<'ast> Visit<'ast> for UnsafeVisitor {
    fn visit_item(&mut self, item: &'ast Item) {
        match item {
            // Unsafe functions
            Item::Fn(func) => {
                if func.sig.unsafety.is_some() {
                    self.add_unsafe(
                        UnsafeKind::UnsafeFunction,
                        func.sig.ident.span().start().line as u32,
                        Some(func.sig.ident.to_string()),
                        "Function declared as unsafe",
                    );
                }
                self.current_function = Some(func.sig.ident.to_string());
                syn::visit::visit_item_fn(self, func);
                self.current_function = None;
            }

            // Unsafe traits
            Item::Trait(trait_def) => {
                if trait_def.unsafety.is_some() {
                    self.add_unsafe(
                        UnsafeKind::UnsafeTrait,
                        trait_def.ident.span().start().line as u32,
                        Some(trait_def.ident.to_string()),
                        "Trait declared as unsafe",
                    );
                }
                syn::visit::visit_item_trait(self, trait_def);
            }

            // Unsafe impl
            Item::Impl(impl_block) => {
                if impl_block.unsafety.is_some() {
                    let name = if let Some((_, path, _)) = &impl_block.trait_ {
                        path.segments.last().map(|s| s.ident.to_string())
                    } else {
                        None
                    };
                    self.add_unsafe(
                        UnsafeKind::UnsafeImpl,
                        impl_block.impl_token.span.start().line as u32,
                        name,
                        "Unsafe trait implementation",
                    );
                }

                // Check methods in impl block
                for impl_item in &impl_block.items {
                    if let ImplItem::Fn(method) = impl_item {
                        if method.sig.unsafety.is_some() {
                            self.add_unsafe(
                                UnsafeKind::UnsafeFunction,
                                method.sig.ident.span().start().line as u32,
                                Some(method.sig.ident.to_string()),
                                "Method declared as unsafe",
                            );
                        }
                    }
                }

                syn::visit::visit_item_impl(self, impl_block);
            }

            // Extern blocks (FFI)
            Item::ForeignMod(foreign) => {
                for foreign_item in &foreign.items {
                    if let syn::ForeignItem::Fn(func) = foreign_item {
                        // Track extern functions so we can detect calls to them
                        self.extern_functions.insert(func.sig.ident.to_string());
                        self.add_unsafe(
                            UnsafeKind::FfiCall,
                            func.sig.ident.span().start().line as u32,
                            Some(func.sig.ident.to_string()),
                            "Foreign function declaration (requires unsafe to call)",
                        );
                    }
                }
            }

            // Mutable statics
            Item::Static(static_item) => {
                if matches!(static_item.mutability, syn::StaticMutability::Mut(_)) {
                    self.add_unsafe(
                        UnsafeKind::MutableStaticAccess,
                        static_item.ident.span().start().line as u32,
                        Some(static_item.ident.to_string()),
                        "Mutable static variable (requires unsafe to access)",
                    );
                }
            }

            // Union types
            Item::Union(union_def) => {
                self.add_unsafe(
                    UnsafeKind::UnionFieldAccess,
                    union_def.ident.span().start().line as u32,
                    Some(union_def.ident.to_string()),
                    "Union type (field access requires unsafe)",
                );
            }

            _ => syn::visit::visit_item(self, item),
        }
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        if let TraitItem::Fn(method) = item {
            if method.sig.unsafety.is_some() {
                self.add_unsafe(
                    UnsafeKind::UnsafeFunction,
                    method.sig.ident.span().start().line as u32,
                    Some(method.sig.ident.to_string()),
                    "Trait method declared as unsafe",
                );
            }
        }
        syn::visit::visit_trait_item(self, item);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            // Unsafe blocks
            Expr::Unsafe(unsafe_expr) => {
                self.add_unsafe(
                    UnsafeKind::UnsafeBlock,
                    unsafe_expr.unsafe_token.span.start().line as u32,
                    self.current_function.clone(),
                    "Unsafe block",
                );

                // Set flag and continue visiting inside the block
                let was_in_unsafe = self.in_unsafe_block;
                self.in_unsafe_block = true;
                syn::visit::visit_expr_unsafe(self, unsafe_expr);
                self.in_unsafe_block = was_in_unsafe;
                return; // Don't double-visit
            }

            // Raw pointer dereference
            Expr::Unary(unary) if self.in_unsafe_block => {
                if matches!(unary.op, syn::UnOp::Deref(_)) {
                    // Check if it's a raw pointer dereference
                    // This is a heuristic - we look for *ptr patterns
                    if let Expr::Path(_) | Expr::Field(_) | Expr::MethodCall(_) =
                        unary.expr.as_ref()
                    {
                        // Could be pointer deref, report as potential
                        self.add_unsafe(
                            UnsafeKind::RawPointerDeref,
                            unary.op.span().start().line as u32,
                            self.current_function.clone(),
                            "Potential raw pointer dereference",
                        );
                    }
                }
            }

            _ => {}
        }

        syn::visit::visit_expr(self, expr);
    }
}

/// Analyze a single Rust file for unsafe constructs
fn analyze_file(path: &Path) -> Result<Vec<UnsafeLocation>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

    let syntax = syn::parse_file(&content)
        .map_err(|e| format!("Failed to parse file {}: {}", path.display(), e))?;

    let mut visitor = UnsafeVisitor::new();
    visitor.visit_file(&syntax);

    Ok(visitor.locations)
}

/// Analyze all Rust files in a directory for unsafe code
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
    let mut files_with_issues = std::collections::HashSet::new();

    // Count by type
    let mut counts: HashMap<UnsafeKind, u32> = HashMap::new();

    for file_path in files {
        files_checked += 1;

        match analyze_file(&file_path) {
            Ok(locations) => {
                if !locations.is_empty() {
                    files_with_issues.insert(file_path.to_string_lossy().to_string());
                }

                for loc in locations {
                    *counts.entry(loc.kind).or_insert(0) += 1;

                    let file_str = file_path.to_string_lossy().to_string();
                    let message = if let Some(ref name) = loc.name {
                        format!("{}: '{}' - {}", loc.kind.as_str(), name, loc.reason)
                    } else {
                        format!("{} - {}", loc.kind.as_str(), loc.reason)
                    };

                    // Unsafe blocks and functions are informational,
                    // unsafe traits/impls and FFI are warnings
                    let severity = match loc.kind {
                        UnsafeKind::UnsafeBlock => IssueSeverity::Info,
                        UnsafeKind::UnsafeFunction => IssueSeverity::Info,
                        UnsafeKind::RawPointerDeref => IssueSeverity::Warning,
                        UnsafeKind::UnsafeFnCall => IssueSeverity::Info,
                        UnsafeKind::MutableStaticAccess => IssueSeverity::Warning,
                        UnsafeKind::UnionFieldAccess => IssueSeverity::Warning,
                        UnsafeKind::UnsafeTrait => IssueSeverity::Warning,
                        UnsafeKind::UnsafeImpl => IssueSeverity::Info,
                        UnsafeKind::FfiCall => IssueSeverity::Warning,
                    };

                    issues.push(
                        IssueBuilder::new(file_str, message)
                            .line(loc.line)
                            .code(format!("UNSAFE_{:?}", loc.kind).to_uppercase())
                            .severity(severity)
                            .build(),
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Error analyzing {}: {}", file_path.display(), e);
            }
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

    // Build tool_data with counts by type
    let mut tool_data: HashMap<String, serde_json::Value> = HashMap::new();
    tool_data.insert(
        "unsafe_functions".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::UnsafeFunction).unwrap_or(&0)),
    );
    tool_data.insert(
        "unsafe_blocks".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::UnsafeBlock).unwrap_or(&0)),
    );
    tool_data.insert(
        "unsafe_traits".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::UnsafeTrait).unwrap_or(&0)),
    );
    tool_data.insert(
        "unsafe_impls".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::UnsafeImpl).unwrap_or(&0)),
    );
    tool_data.insert(
        "mutable_statics".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::MutableStaticAccess).unwrap_or(&0)),
    );
    tool_data.insert(
        "ffi_declarations".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::FfiCall).unwrap_or(&0)),
    );
    tool_data.insert(
        "unions".to_string(),
        serde_json::json!(*counts.get(&UnsafeKind::UnionFieldAccess).unwrap_or(&0)),
    );
    tool_data.insert("total_unsafe".to_string(), serde_json::json!(issues_found));

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
            tool_data,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_analyze(code: &str) -> Vec<UnsafeLocation> {
        let syntax = syn::parse_file(code).unwrap();
        let mut visitor = UnsafeVisitor::new();
        visitor.visit_file(&syntax);
        visitor.locations
    }

    #[test]
    fn test_unsafe_function() {
        let code = r#"
            unsafe fn dangerous() {
                // unsafe code
            }
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations
                .iter()
                .any(|l| l.kind == UnsafeKind::UnsafeFunction),
            "Should detect unsafe function"
        );
    }

    #[test]
    fn test_unsafe_block() {
        let code = r#"
            fn safe() {
                unsafe {
                    // unsafe operations
                }
            }
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations.iter().any(|l| l.kind == UnsafeKind::UnsafeBlock),
            "Should detect unsafe block"
        );
    }

    #[test]
    fn test_unsafe_trait() {
        let code = r#"
            unsafe trait MyTrait {
                fn method(&self);
            }
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations.iter().any(|l| l.kind == UnsafeKind::UnsafeTrait),
            "Should detect unsafe trait"
        );
    }

    #[test]
    fn test_unsafe_impl() {
        let code = r#"
            unsafe impl Send for MyType {}
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations.iter().any(|l| l.kind == UnsafeKind::UnsafeImpl),
            "Should detect unsafe impl"
        );
    }

    #[test]
    fn test_mutable_static() {
        let code = r#"
            static mut COUNTER: u32 = 0;
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations
                .iter()
                .any(|l| l.kind == UnsafeKind::MutableStaticAccess),
            "Should detect mutable static"
        );
    }

    #[test]
    fn test_extern_fn() {
        let code = r#"
            extern "C" {
                fn external_function();
            }
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations.iter().any(|l| l.kind == UnsafeKind::FfiCall),
            "Should detect FFI declaration"
        );
    }

    #[test]
    fn test_union() {
        let code = r#"
            union MyUnion {
                a: u32,
                b: f32,
            }
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations
                .iter()
                .any(|l| l.kind == UnsafeKind::UnionFieldAccess),
            "Should detect union"
        );
    }

    #[test]
    fn test_safe_code() {
        let code = r#"
            fn safe_function() {
                let x = 42;
                println!("{}", x);
            }
        "#;

        let locations = parse_and_analyze(code);
        assert!(
            locations.is_empty(),
            "Safe code should have no unsafe locations"
        );
    }
}
