//! Synchronization primitive detection for Rust
//!
//! Detects Mutex, RwLock, and other synchronization primitives.

#![allow(dead_code)]

use crate::check_executor::analyzers::concurrency::types::{AnalysisContext, LockInfo, LockType};
use syn::{File, Item, ItemStruct};

use super::shared_state::type_to_string;

/// Detect synchronization primitives in a Rust file
pub fn detect_sync_primitives(file: &File, ctx: &mut AnalysisContext) {
    for item in &file.items {
        detect_item_sync(item, ctx);
    }
}

/// Detect sync primitives in a single item
fn detect_item_sync(item: &Item, ctx: &mut AnalysisContext) {
    match item {
        // Struct fields might contain Mutex/RwLock
        Item::Struct(struct_item) => {
            detect_struct_sync(struct_item, ctx);
        }
        // Static variables with sync types
        Item::Static(static_item) => {
            let name = static_item.ident.to_string();
            let type_str = type_to_string(&static_item.ty);

            if let Some(lock_type) = detect_lock_type_from_string(&type_str) {
                ctx.locks.push(LockInfo {
                    name,
                    lock_type,
                    definition_line: static_item.ident.span().start().line as u32,
                    protects: Vec::new(),
                });
            }
        }
        // Const items (less common but possible)
        Item::Const(const_item) => {
            let type_str = type_to_string(&const_item.ty);

            if let Some(lock_type) = detect_lock_type_from_string(&type_str) {
                ctx.locks.push(LockInfo {
                    name: const_item.ident.to_string(),
                    lock_type,
                    definition_line: const_item.ident.span().start().line as u32,
                    protects: Vec::new(),
                });
            }
        }
        // Modules
        Item::Mod(module) => {
            if let Some((_, items)) = &module.content {
                for item in items {
                    detect_item_sync(item, ctx);
                }
            }
        }
        _ => {}
    }
}

/// Detect sync primitives in struct fields
fn detect_struct_sync(struct_item: &ItemStruct, ctx: &mut AnalysisContext) {
    if let syn::Fields::Named(fields) = &struct_item.fields {
        for field in &fields.named {
            if let Some(ident) = &field.ident {
                let field_name = ident.to_string();
                let type_str = type_to_string(&field.ty);

                if let Some(lock_type) = detect_lock_type_from_string(&type_str) {
                    let full_name = format!("{}.{}", struct_item.ident, field_name);
                    ctx.locks.push(LockInfo {
                        name: full_name,
                        lock_type,
                        definition_line: ident.span().start().line as u32,
                        protects: infer_protected_fields(&field_name, struct_item),
                    });
                }
            }
        }
    }
}

/// Detect lock type from a type string
fn detect_lock_type_from_string(type_str: &str) -> Option<LockType> {
    if type_str.contains("Mutex") {
        if type_str.contains("parking_lot") {
            Some(LockType::ParkingLotMutex)
        } else if type_str.contains("tokio") {
            Some(LockType::TokioMutex)
        } else {
            Some(LockType::StdMutex)
        }
    } else if type_str.contains("RwLock") {
        Some(LockType::StdRwLock)
    } else {
        None
    }
}

/// Infer which fields a lock might protect based on naming
fn infer_protected_fields(lock_field: &str, struct_item: &ItemStruct) -> Vec<String> {
    let mut protected = Vec::new();

    // Extract base name from lock field (e.g., "data_lock" -> "data")
    let base_name = lock_field
        .strip_suffix("_lock")
        .or_else(|| lock_field.strip_suffix("_mutex"))
        .or_else(|| lock_field.strip_suffix("Lock"))
        .or_else(|| lock_field.strip_suffix("Mutex"))
        .unwrap_or(lock_field);

    if let syn::Fields::Named(fields) = &struct_item.fields {
        for field in &fields.named {
            if let Some(ident) = &field.ident {
                let field_name = ident.to_string();

                // Check for matching names
                if field_name == base_name
                    || field_name.starts_with(&format!("{}_", base_name))
                    || field_name.ends_with(&format!("_{}", base_name))
                {
                    protected.push(field_name);
                }
            }
        }
    }

    protected
}

/// Check if a type is a synchronization primitive
pub fn is_sync_type(type_str: &str) -> bool {
    type_str.contains("Mutex")
        || type_str.contains("RwLock")
        || type_str.contains("Semaphore")
        || type_str.contains("Condvar")
        || type_str.contains("Barrier")
        || type_str.contains("channel")
}

/// Check if a type is inherently thread-safe
pub fn is_thread_safe_type(type_str: &str) -> bool {
    type_str.contains("Atomic")
        || type_str.contains("Arc")
        || type_str.contains("OnceCell")
        || type_str.contains("OnceLock")
        || type_str.contains("Sender")
        || type_str.contains("Receiver")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rust(code: &str) -> File {
        syn::parse_file(code).unwrap()
    }

    #[test]
    fn test_detect_mutex_field() {
        let code = r#"
use std::sync::Mutex;

struct Counter {
    count: Mutex<i32>,
}
"#;
        let file = parse_rust(code);
        let mut ctx = AnalysisContext::new();
        detect_sync_primitives(&file, &mut ctx);

        assert_eq!(ctx.locks.len(), 1);
        assert_eq!(ctx.locks[0].name, "Counter.count");
        assert_eq!(ctx.locks[0].lock_type, LockType::StdMutex);
    }

    #[test]
    fn test_detect_rwlock() {
        let code = r#"
use std::sync::RwLock;

static CACHE: RwLock<Vec<String>> = RwLock::new(Vec::new());
"#;
        let file = parse_rust(code);
        let mut ctx = AnalysisContext::new();
        detect_sync_primitives(&file, &mut ctx);

        assert_eq!(ctx.locks.len(), 1);
        assert_eq!(ctx.locks[0].name, "CACHE");
        assert_eq!(ctx.locks[0].lock_type, LockType::StdRwLock);
    }

    #[test]
    fn test_infer_protected_fields() {
        let code = r#"
struct DataHolder {
    data: Vec<i32>,
    data_lock: std::sync::Mutex<()>,
}
"#;
        let file = parse_rust(code);

        if let Item::Struct(struct_item) = &file.items[0] {
            let protected = infer_protected_fields("data_lock", struct_item);
            assert!(protected.contains(&"data".to_string()));
        }
    }

    #[test]
    fn test_is_sync_type() {
        assert!(is_sync_type("std::sync::Mutex<i32>"));
        assert!(is_sync_type("parking_lot::RwLock<Vec<String>>"));
        assert!(!is_sync_type("Vec<i32>"));
        assert!(!is_sync_type("String"));
    }

    #[test]
    fn test_is_thread_safe_type() {
        assert!(is_thread_safe_type("AtomicUsize"));
        assert!(is_thread_safe_type("Arc<Mutex<i32>>"));
        assert!(is_thread_safe_type("OnceCell<Config>"));
        assert!(!is_thread_safe_type("RefCell<i32>"));
    }
}
