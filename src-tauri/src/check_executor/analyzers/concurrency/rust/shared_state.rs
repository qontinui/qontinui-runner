//! Shared state detection for Rust
//!
//! Detects:
//! - `static mut` variables (inherently unsafe for concurrent access)
//! - `lazy_static!` globals
//! - `OnceCell` and `OnceLock` usage
//! - `Arc<Mutex<T>>` and `Arc<RwLock<T>>` patterns

use crate::check_executor::analyzers::concurrency::types::{
    AccessType, AnalysisContext, SharedState, SharedStateType, StateAccess,
};
use syn::visit::Visit;
use syn::{Block, Expr, File, Item, ItemStatic, Type};

/// Convert a syn::Type to a string representation
pub fn type_to_string(ty: &Type) -> String {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .iter()
            .map(|seg| {
                let ident = seg.ident.to_string();
                match &seg.arguments {
                    syn::PathArguments::None => ident,
                    syn::PathArguments::AngleBracketed(args) => {
                        let args_str: Vec<String> = args
                            .args
                            .iter()
                            .filter_map(|arg| match arg {
                                syn::GenericArgument::Type(t) => Some(type_to_string(t)),
                                _ => None,
                            })
                            .collect();
                        if args_str.is_empty() {
                            ident
                        } else {
                            format!("{}<{}>", ident, args_str.join(", "))
                        }
                    }
                    syn::PathArguments::Parenthesized(_) => ident,
                }
            })
            .collect::<Vec<_>>()
            .join("::"),
        Type::Reference(type_ref) => {
            let inner = type_to_string(&type_ref.elem);
            if type_ref.mutability.is_some() {
                format!("&mut {}", inner)
            } else {
                format!("&{}", inner)
            }
        }
        Type::Tuple(tuple) => {
            let types: Vec<String> = tuple.elems.iter().map(type_to_string).collect();
            format!("({})", types.join(", "))
        }
        Type::Array(arr) => {
            format!("[{}; _]", type_to_string(&arr.elem))
        }
        Type::Slice(slice) => {
            format!("[{}]", type_to_string(&slice.elem))
        }
        _ => "unknown".to_string(),
    }
}

/// Detect shared state in a Rust file
pub fn detect_shared_state(file: &File, file_path: &str, ctx: &mut AnalysisContext) {
    for item in &file.items {
        detect_item_shared_state(item, file_path, ctx);
    }
}

/// Detect shared state in a single item
fn detect_item_shared_state(item: &Item, file_path: &str, ctx: &mut AnalysisContext) {
    match item {
        // Static variables
        Item::Static(static_item) => {
            detect_static_state(static_item, file_path, ctx);
        }
        // Macro invocations (for lazy_static!, once_cell, etc.)
        Item::Macro(macro_item) => {
            if let Some(ident) = &macro_item.ident {
                let name = ident.to_string();
                // Note: We can't easily parse lazy_static! contents, but we mark it
                ctx.shared_states.push(SharedState {
                    name,
                    state_type: SharedStateType::LazyStatic,
                    definition_line: macro_item
                        .mac
                        .path
                        .segments
                        .first()
                        .map(|s| s.ident.span().start().line as u32)
                        .unwrap_or(1),
                    file: file_path.to_string(),
                    accesses: Vec::new(),
                    appears_thread_safe: true, // lazy_static is typically thread-safe
                    thread_safe_reason: Some(
                        "lazy_static! provides thread-safe initialization".to_string(),
                    ),
                });
            } else {
                // Anonymous macro - check if it's lazy_static
                let macro_name = macro_item
                    .mac
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::");

                if macro_name.contains("lazy_static") {
                    ctx.shared_states.push(SharedState {
                        name: "lazy_static_block".to_string(),
                        state_type: SharedStateType::LazyStatic,
                        definition_line: macro_item
                            .mac
                            .path
                            .segments
                            .first()
                            .map(|s| s.ident.span().start().line as u32)
                            .unwrap_or(1),
                        file: file_path.to_string(),
                        accesses: Vec::new(),
                        appears_thread_safe: true,
                        thread_safe_reason: Some(
                            "lazy_static! provides thread-safe initialization".to_string(),
                        ),
                    });
                }
            }
        }
        // Modules
        Item::Mod(module) => {
            if let Some((_, items)) = &module.content {
                for item in items {
                    detect_item_shared_state(item, file_path, ctx);
                }
            }
        }
        _ => {}
    }
}

/// Detect static variable state
fn detect_static_state(static_item: &ItemStatic, file_path: &str, ctx: &mut AnalysisContext) {
    let name = static_item.ident.to_string();
    let line = static_item.ident.span().start().line as u32;

    let is_mutable = matches!(static_item.mutability, syn::StaticMutability::Mut(_));
    let (state_type, appears_safe, reason) = if is_mutable {
        // `static mut` - inherently unsafe
        (SharedStateType::StaticMut, false, None)
    } else {
        // Regular static - check the type for thread-safety
        let type_str = type_to_string(&static_item.ty);
        classify_static_type(&type_str)
    };

    ctx.shared_states.push(SharedState {
        name,
        state_type,
        definition_line: line,
        file: file_path.to_string(),
        accesses: Vec::new(),
        appears_thread_safe: appears_safe,
        thread_safe_reason: reason,
    });
}

/// Classify a static type for thread-safety
fn classify_static_type(type_str: &str) -> (SharedStateType, bool, Option<String>) {
    // OnceCell / OnceLock
    if type_str.contains("OnceCell") || type_str.contains("OnceLock") {
        return (
            SharedStateType::OnceCell,
            true,
            Some("OnceCell/OnceLock provides thread-safe single initialization".to_string()),
        );
    }

    // Atomic types
    if type_str.contains("Atomic") {
        return (
            SharedStateType::Other("atomic".to_string()),
            true,
            Some("Atomic types are inherently thread-safe".to_string()),
        );
    }

    // Mutex wrapped
    if type_str.contains("Mutex") {
        if type_str.contains("Arc") {
            return (
                SharedStateType::ArcMutex,
                true,
                Some("Arc<Mutex<T>> is thread-safe when properly used".to_string()),
            );
        }
        return (
            SharedStateType::Other("mutex".to_string()),
            true,
            Some("Mutex<T> provides thread-safe access".to_string()),
        );
    }

    // RwLock wrapped
    if type_str.contains("RwLock") {
        if type_str.contains("Arc") {
            return (
                SharedStateType::ArcRwLock,
                true,
                Some("Arc<RwLock<T>> is thread-safe when properly used".to_string()),
            );
        }
        return (
            SharedStateType::Other("rwlock".to_string()),
            true,
            Some("RwLock<T> provides thread-safe access".to_string()),
        );
    }

    // Default to potentially unsafe
    (SharedStateType::Other("static".to_string()), false, None)
}

/// Analyze a function body for state accesses
pub fn analyze_function_body(block: &Block, file_path: &str, ctx: &mut AnalysisContext) {
    let mut visitor = AccessVisitor {
        ctx,
        file_path,
        in_unsafe: false,
        in_lock_guard: false,
        current_function: None,
    };
    visitor.visit_block(block);
}

/// Visitor for finding state accesses
struct AccessVisitor<'a> {
    ctx: &'a mut AnalysisContext,
    file_path: &'a str,
    in_unsafe: bool,
    in_lock_guard: bool,
    current_function: Option<String>,
}

impl<'a> Visit<'a> for AccessVisitor<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            // Track unsafe blocks
            Expr::Unsafe(_unsafe_expr) => {
                let was_unsafe = self.in_unsafe;
                self.in_unsafe = true;
                syn::visit::visit_expr(self, expr);
                self.in_unsafe = was_unsafe;
                return;
            }

            // Check for identifier access (might be static variable)
            Expr::Path(path_expr) => {
                if let Some(ident) = path_expr.path.get_ident() {
                    let name = ident.to_string();
                    self.check_state_access(&name, ident.span().start().line as u32);
                }
            }

            // Check for method calls (might be lock().unwrap())
            Expr::MethodCall(method_call) => {
                let method_name = method_call.method.to_string();
                if method_name == "lock" || method_name == "read" || method_name == "write" {
                    // This might be acquiring a lock
                    self.in_lock_guard = true;
                }
            }

            // Assignments
            Expr::Assign(assign) => {
                // The left side is a write
                if let Expr::Path(path) = assign.left.as_ref() {
                    if let Some(ident) = path.path.get_ident() {
                        let name = ident.to_string();
                        self.record_access(
                            &name,
                            ident.span().start().line as u32,
                            AccessType::Write,
                        );
                    }
                }
            }

            // Compound assignments (+=, -=, etc.)
            Expr::Binary(binary) => {
                use syn::BinOp;
                if matches!(
                    binary.op,
                    BinOp::AddAssign(_)
                        | BinOp::SubAssign(_)
                        | BinOp::MulAssign(_)
                        | BinOp::DivAssign(_)
                        | BinOp::BitAndAssign(_)
                        | BinOp::BitOrAssign(_)
                        | BinOp::BitXorAssign(_)
                ) {
                    if let Expr::Path(path) = binary.left.as_ref() {
                        if let Some(ident) = path.path.get_ident() {
                            let name = ident.to_string();
                            self.record_access(
                                &name,
                                ident.span().start().line as u32,
                                AccessType::Compound,
                            );
                        }
                    }
                }
            }

            _ => {}
        }

        // Continue visiting
        syn::visit::visit_expr(self, expr);
    }

    fn visit_local(&mut self, local: &'a syn::Local) {
        // Check for lock guard patterns: let _guard = mutex.lock()
        if let Some(init) = &local.init {
            if let Expr::MethodCall(method_call) = init.expr.as_ref() {
                let method_name = method_call.method.to_string();
                if method_name == "lock" || method_name == "read" || method_name == "write" {
                    self.in_lock_guard = true;
                }
            }
        }
        syn::visit::visit_local(self, local);
    }
}

impl<'a> AccessVisitor<'a> {
    fn check_state_access(&mut self, name: &str, line: u32) {
        // Check if this is a known shared state
        if self.ctx.shared_states.iter().any(|s| s.name == name) {
            self.record_access(name, line, AccessType::Read);
        }
    }

    fn record_access(&mut self, name: &str, line: u32, access_type: AccessType) {
        if let Some(state) = self.ctx.find_state_mut(name) {
            let access = StateAccess {
                state_name: name.to_string(),
                access_type,
                line,
                column: None,
                is_protected: self.in_lock_guard || !self.in_unsafe,
                lock_name: None,
                context: self.current_function.clone(),
            };
            state.accesses.push(access);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rust(code: &str) -> File {
        syn::parse_file(code).unwrap()
    }

    #[test]
    fn test_detect_static_mut() {
        let code = r#"
static mut COUNTER: i32 = 0;
"#;
        let file = parse_rust(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&file, "test.rs", &mut ctx);

        assert_eq!(ctx.shared_states.len(), 1);
        assert_eq!(ctx.shared_states[0].name, "COUNTER");
        assert_eq!(ctx.shared_states[0].state_type, SharedStateType::StaticMut);
        assert!(!ctx.shared_states[0].appears_thread_safe);
    }

    #[test]
    fn test_detect_atomic() {
        let code = r#"
use std::sync::atomic::AtomicUsize;

static COUNTER: AtomicUsize = AtomicUsize::new(0);
"#;
        let file = parse_rust(code);
        let mut ctx = AnalysisContext::new();
        detect_shared_state(&file, "test.rs", &mut ctx);

        assert_eq!(ctx.shared_states.len(), 1);
        assert!(ctx.shared_states[0].appears_thread_safe);
    }
}
