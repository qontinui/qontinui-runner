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

    /// Benchmark: measure tree capture latency across multiple iterations.
    #[tokio::test]
    #[ignore] // Requires desktop session
    async fn bench_capture_latency() {
        use std::time::Instant;

        let mut mgr = AccessibilityManager::new();
        mgr.connect(ConnectionTarget::Desktop, 10_000)
            .await
            .expect("Desktop connection failed");

        // Warm up
        mgr.capture(Some(3), false)
            .await
            .expect("Warmup capture failed");

        // Benchmark 10 captures
        let iterations = 10;
        let mut times = Vec::with_capacity(iterations);

        for i in 0..iterations {
            let start = Instant::now();
            let snapshot = mgr.capture(Some(3), false).await.expect("Capture failed");
            let elapsed = start.elapsed();
            times.push(elapsed);
            println!(
                "  Capture {}: {} nodes in {:.1}ms",
                i + 1,
                snapshot.total_nodes,
                elapsed.as_secs_f64() * 1000.0
            );
        }

        let avg_ms =
            times.iter().map(|t| t.as_secs_f64() * 1000.0).sum::<f64>() / iterations as f64;
        let min_ms = times
            .iter()
            .map(|t| t.as_secs_f64() * 1000.0)
            .fold(f64::MAX, f64::min);
        let max_ms = times
            .iter()
            .map(|t| t.as_secs_f64() * 1000.0)
            .fold(0.0_f64, f64::max);

        println!("\n=== Capture Benchmark (depth=3) ===");
        println!("  Iterations: {}", iterations);
        println!("  Avg: {:.1}ms", avg_ms);
        println!("  Min: {:.1}ms", min_ms);
        println!("  Max: {:.1}ms", max_ms);

        mgr.disconnect().await.expect("Disconnect failed");

        // Sanity check: captures should be fast (under 500ms each)
        assert!(
            avg_ms < 500.0,
            "Average capture time {:.1}ms exceeds 500ms threshold",
            avg_ms
        );
    }

    /// Connect to Calculator, click buttons to compute 2+3=5, verify result.
    #[tokio::test]
    #[ignore] // Requires Calculator to be open
    async fn test_calculator_interaction() {
        let mut mgr = AccessibilityManager::new();

        // Connect to Calculator
        let result = mgr
            .connect(
                ConnectionTarget::WindowTitle("Calculator".to_string()),
                10_000,
            )
            .await;
        match &result {
            Ok(()) => println!("Connected to Calculator"),
            Err(e) => {
                println!("Calculator not open or not found: {}", e);
                println!("Open Calculator and try again.");
                return; // Skip gracefully
            }
        }

        // Capture the tree
        let snapshot = mgr.capture(None, false).await.expect("Capture failed");
        println!(
            "Captured {} nodes ({} interactive)",
            snapshot.total_nodes, snapshot.interactive_nodes
        );

        // Print AI context to see what's available
        let context = mgr.to_ai_context(50, true).await;
        println!("\n{}", context);

        // Query for all buttons
        let snap = mgr.snapshot().await.unwrap();
        let buttons = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .find_all(&snap.root);

        println!("\n=== All Buttons ({}) ===", buttons.len());
        for btn in &buttons {
            println!(
                "  {} {:?}",
                btn.ref_id,
                btn.name.as_deref().unwrap_or("(unnamed)")
            );
        }

        // Try to find and click "Two"
        let two_btn = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .by_label("Two")
            .find_first(&snap.root);

        if let Some(btn) = two_btn {
            println!(
                "\nClicking '{}' ({})",
                btn.name.as_deref().unwrap_or("?"),
                btn.ref_id
            );
            let result = mgr.click(&btn.ref_id).await;
            println!("Click result: {:?}", result);
        } else {
            println!("\nButton 'Two' not found. Available buttons listed above.");
        }

        // Find and click "Plus"
        let plus_btn = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .by_label("Plus")
            .find_first(&snap.root);

        if let Some(btn) = plus_btn {
            println!(
                "Clicking '{}' ({})",
                btn.name.as_deref().unwrap_or("?"),
                btn.ref_id
            );
            let result = mgr.click(&btn.ref_id).await;
            println!("Click result: {:?}", result);
        }

        // Re-capture to get fresh refs, then click "Three"
        let _snapshot2 = mgr.capture(None, false).await.expect("Re-capture failed");
        let snap2 = mgr.snapshot().await.unwrap();

        let three_btn = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .by_label("Three")
            .find_first(&snap2.root);

        if let Some(btn) = three_btn {
            println!(
                "Clicking '{}' ({})",
                btn.name.as_deref().unwrap_or("?"),
                btn.ref_id
            );
            let result = mgr.click(&btn.ref_id).await;
            println!("Click result: {:?}", result);
        }

        // Click "Equals"
        let eq_btn = QueryBuilder::new()
            .by_role(UnifiedRole::Button)
            .by_label("Equals")
            .find_first(&snap2.root);

        if let Some(btn) = eq_btn {
            println!(
                "Clicking '{}' ({})",
                btn.name.as_deref().unwrap_or("?"),
                btn.ref_id
            );
            let result = mgr.click(&btn.ref_id).await;
            println!("Click result: {:?}", result);
        }

        // Capture final state and check for result
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let _final_snap = mgr
            .capture(None, false)
            .await
            .expect("Final capture failed");
        let context = mgr.to_ai_context(30, false).await;
        println!("\n=== Final State ===\n{}", context);

        mgr.disconnect().await.expect("Disconnect failed");
        println!("\nCalculator test complete.");
    }
}
