//! Integration tests for the accessibility layer.
//!
//! These tests require a real desktop session and running applications.
//! They are marked `#[ignore]` so CI skips them. Run manually with:
//!
//! ```bash
//! cargo test --lib accessibility::integration_test -- --ignored --nocapture
//! ```

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use crate::accessibility::model::*;
    use crate::accessibility::query::QueryBuilder;
    use crate::accessibility::traits::ConnectionTarget;
    use crate::accessibility::AccessibilityManager;

    /// Connect to Notepad, capture its accessibility tree, and print the AI context.
    #[tokio::test]
    #[ignore] // Requires Notepad to be open
    async fn test_notepad_capture() {
        let mut mgr = AccessibilityManager::new();

        // Connect to Notepad by window title
        let result = mgr
            .connect(ConnectionTarget::WindowTitle("Notepad".to_string()), 10_000)
            .await;

        match &result {
            Ok(()) => println!("Connected to Notepad via {}", mgr.backend_name()),
            Err(e) => {
                println!("Failed to connect to Notepad: {}", e);
                println!("Make sure Notepad is open.");
                panic!("Connection failed: {}", e);
            }
        }

        // Capture the tree
        let snapshot = mgr.capture(None, false).await.expect("Tree capture failed");

        println!("\n=== Snapshot Summary ===");
        println!("Title: {:?}", snapshot.title);
        println!("Total nodes: {}", snapshot.total_nodes);
        println!("Interactive nodes: {}", snapshot.interactive_nodes);
        println!("Source: {:?}", snapshot.source);
        println!("Generation: {}", snapshot.generation);

        // Print AI context
        let context = mgr.to_ai_context(50, true).await;
        println!("\n=== AI Context ===\n{}", context);

        // Verify we got something
        assert!(snapshot.total_nodes > 0, "Should have captured nodes");
        assert!(
            snapshot.interactive_nodes > 0,
            "Should have found interactive nodes"
        );

        // Query for buttons
        let snap = mgr.snapshot().await.expect("Snapshot should be cached");
        let buttons = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .find_all(&snap.root);
        println!("\n=== Buttons ({}) ===", buttons.len());
        for btn in &buttons {
            println!(
                "  {} {:?} (interactive={})",
                btn.ref_id,
                btn.name.as_deref().unwrap_or("(unnamed)"),
                btn.is_interactive
            );
        }

        // Query for text inputs
        let edits = QueryBuilder::new()
            .by_roles(vec![UnifiedRole::Edit, UnifiedRole::Textbox])
            .find_all(&snap.root);
        println!("\n=== Text Inputs ({}) ===", edits.len());
        for edit in &edits {
            println!(
                "  {} {:?} value={:?}",
                edit.ref_id,
                edit.name.as_deref().unwrap_or("(unnamed)"),
                edit.value.as_deref().unwrap_or("")
            );
        }

        // Try to find the main edit area and type into it
        if let Some(edit) = edits.first() {
            println!("\n=== Typing into {} ===", edit.ref_id);

            // Click to focus
            let focus_result = mgr.focus(&edit.ref_id).await;
            println!("Focus result: {:?}", focus_result);

            // Type some text
            let type_result = mgr
                .type_text(&edit.ref_id, "Hello from Rust accessibility layer!", false)
                .await;
            println!("Type result: {:?}", type_result);
        } else {
            println!("\nNo text input found to type into.");
        }

        // Disconnect
        mgr.disconnect().await.expect("Disconnect failed");
        println!("\nDisconnected. Test complete.");
    }

    /// Connect to Desktop root and list top-level windows.
    #[tokio::test]
    #[ignore] // Requires desktop session
    async fn test_desktop_window_listing() {
        let mut mgr = AccessibilityManager::new();

        mgr.connect(ConnectionTarget::Desktop, 10_000)
            .await
            .expect("Desktop connection failed");

        let snapshot = mgr
            .capture(Some(2), false)
            .await
            .expect("Desktop capture failed");

        println!("\n=== Desktop Windows ===");
        println!("Total nodes (depth 2): {}", snapshot.total_nodes);

        for child in &snapshot.root.children {
            let name = child.name.as_deref().unwrap_or("(unnamed)");
            let role = child.role.as_str();
            let class = child.class_name.as_deref().unwrap_or("");
            println!("  [{}] {} (class: {})", role, name, class);
        }

        mgr.disconnect().await.expect("Disconnect failed");
    }
}
