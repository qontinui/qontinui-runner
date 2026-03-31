//! Schema Export Binary
//!
//! Prints all registered JSON Schemas to stdout as a single JSON object.
//! Used by the type generation pipeline to produce TypeScript and Python types.
//!
//! Usage:
//!   cargo run --bin export_schemas > schemas.json
//!   cargo run --bin export_schemas -- --pretty > schemas.json
//!   cargo run --bin export_schemas -- --list
//!
//! The output is a JSON object where keys are type names and values are
//! full JSON Schema (draft 2020-12) objects.

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let schemas = qontinui_runner_lib::schema_export::export_all_schemas();

    if args.iter().any(|a| a == "--list") {
        // List mode: just print type names
        if let Some(obj) = schemas.as_object() {
            for key in obj.keys() {
                println!("{}", key);
            }
        }
        return;
    }

    let pretty = args.iter().any(|a| a == "--pretty");

    let output = if pretty {
        serde_json::to_string_pretty(&schemas).expect("Failed to serialize schemas")
    } else {
        serde_json::to_string(&schemas).expect("Failed to serialize schemas")
    };

    println!("{}", output);
}
