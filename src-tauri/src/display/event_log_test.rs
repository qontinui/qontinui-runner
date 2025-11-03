// Unit tests for EventLog and ActionLogProfile
// These tests verify the Display Profile framework works correctly

#[cfg(test)]
mod tests {
    use crate::display::{EventLog, RawEvent};
    use crate::display::profiles::ActionLogProfile;
    use crate::display::profile::DisplayProfile;
    use serde_json::json;

    #[test]
    fn test_event_log_stores_events() {
        println!("\n[TEST] EventLog stores events");

        let mut event_log = EventLog::new();

        // Add a tree event (action_started)
        let event = RawEvent {
            id: "test-1".to_string(),
            event_type: "action_started".to_string(),
            timestamp: 1234567890.0,
            data: json!({
                "node": {
                    "id": "action-1",
                    "node_type": "action",
                    "name": "CLICK",
                    "timestamp": 1234567890.0,
                    "status": "running",
                    "metadata": {}
                }
            }),
            sequence: 1,
        };

        event_log.add_event(event);

        // Verify event was stored
        assert_eq!(event_log.len(), 1, "EventLog should have 1 event");

        let events = event_log.events();
        assert_eq!(events[0].event_type, "action_started");
        assert_eq!(events[0].sequence, 1);

        println!("✓ PASS: EventLog stores events correctly");
    }

    #[test]
    fn test_action_log_profile_processes_click_action() {
        println!("\n[TEST] ActionLogProfile processes CLICK action");

        let mut event_log = EventLog::new();

        // Add action_started event
        event_log.add_event(RawEvent {
            id: "test-1".to_string(),
            event_type: "action_started".to_string(),
            timestamp: 1234567890.0,
            data: json!({
                "node": {
                    "id": "action-1",
                    "node_type": "action",
                    "name": "CLICK",
                    "timestamp": 1234567890.0,
                    "status": "running",
                    "metadata": {"config": {"x": 100, "y": 200}}
                }
            }),
            sequence: 1,
        });

        // Add action_completed event
        event_log.add_event(RawEvent {
            id: "test-2".to_string(),
            event_type: "action_completed".to_string(),
            timestamp: 1234567891.0,
            data: json!({
                "node": {
                    "id": "action-1",
                    "node_type": "action",
                    "name": "CLICK",
                    "timestamp": 1234567891.0,
                    "end_timestamp": 1234567891.0,
                    "duration": 1.0,
                    "status": "success",
                    "metadata": {}
                }
            }),
            sequence: 2,
        });

        // Process events with ActionLogProfile
        let profile = ActionLogProfile::with_default_config();
        let events = event_log.events();
        let view_data = profile.process(events);

        // Verify CLICK action appears in view data
        assert!(view_data.visible_count > 0, "ActionLogProfile should return visible actions");
        assert!(view_data.actions.len() > 0, "ActionLogProfile should return action entries");

        let first_action = &view_data.actions[0];
        assert_eq!(first_action.action_type, "CLICK");
        assert_eq!(first_action.status, "success");
        assert!(first_action.duration.is_some());

        println!("✓ PASS: ActionLogProfile processes CLICK action correctly");
        println!("  Actions returned: {}", view_data.actions.len());
        println!("  First action: type={}, status={}", first_action.action_type, first_action.status);
    }

    #[test]
    fn test_action_log_profile_excludes_if_actions() {
        println!("\n[TEST] ActionLogProfile excludes IF actions");

        let mut event_log = EventLog::new();

        // Add IF action
        event_log.add_event(RawEvent {
            id: "test-if-1".to_string(),
            event_type: "action_started".to_string(),
            timestamp: 1234567890.0,
            data: json!({
                "node": {
                    "id": "if-1",
                    "node_type": "action",
                    "name": "IF",
                    "timestamp": 1234567890.0,
                    "status": "running",
                    "metadata": {}
                }
            }),
            sequence: 1,
        });

        event_log.add_event(RawEvent {
            id: "test-if-2".to_string(),
            event_type: "action_completed".to_string(),
            timestamp: 1234567891.0,
            data: json!({
                "node": {
                    "id": "if-1",
                    "node_type": "action",
                    "name": "IF",
                    "timestamp": 1234567891.0,
                    "status": "success",
                    "metadata": {}
                }
            }),
            sequence: 2,
        });

        // Add CLICK action (should be visible)
        event_log.add_event(RawEvent {
            id: "test-click-1".to_string(),
            event_type: "action_started".to_string(),
            timestamp: 1234567892.0,
            data: json!({
                "node": {
                    "id": "click-1",
                    "node_type": "action",
                    "name": "CLICK",
                    "timestamp": 1234567892.0,
                    "status": "running",
                    "metadata": {}
                }
            }),
            sequence: 3,
        });

        event_log.add_event(RawEvent {
            id: "test-click-2".to_string(),
            event_type: "action_completed".to_string(),
            timestamp: 1234567893.0,
            data: json!({
                "node": {
                    "id": "click-1",
                    "node_type": "action",
                    "name": "CLICK",
                    "timestamp": 1234567893.0,
                    "status": "success",
                    "metadata": {}
                }
            }),
            sequence: 4,
        });

        // Process with profile
        let profile = ActionLogProfile::with_default_config();
        let events = event_log.events();
        let view_data = profile.process(events);

        // Verify IF action is excluded, CLICK action is included
        assert_eq!(view_data.visible_count, 1, "Should have 1 visible action (CLICK)");
        assert_eq!(view_data.total_count, 2, "Should have 2 total actions (IF + CLICK)");
        assert_eq!(view_data.actions.len(), 1, "Should return 1 action (CLICK)");
        assert_eq!(view_data.actions[0].action_type, "CLICK");

        println!("✓ PASS: ActionLogProfile correctly excludes IF actions");
        println!("  Total actions: {}", view_data.total_count);
        println!("  Visible actions: {}", view_data.visible_count);
    }

    #[test]
    fn test_action_log_profile_excludes_wait_actions() {
        println!("\n[TEST] ActionLogProfile excludes WAIT actions");

        let mut event_log = EventLog::new();

        // Add WAIT action
        event_log.add_event(RawEvent {
            id: "test-wait-1".to_string(),
            event_type: "action_started".to_string(),
            timestamp: 1234567890.0,
            data: json!({
                "node": {
                    "id": "wait-1",
                    "node_type": "action",
                    "name": "WAIT",
                    "timestamp": 1234567890.0,
                    "status": "running",
                    "metadata": {}
                }
            }),
            sequence: 1,
        });

        event_log.add_event(RawEvent {
            id: "test-wait-2".to_string(),
            event_type: "action_completed".to_string(),
            timestamp: 1234567891.0,
            data: json!({
                "node": {
                    "id": "wait-1",
                    "node_type": "action",
                    "name": "WAIT",
                    "timestamp": 1234567891.0,
                    "status": "success",
                    "metadata": {}
                }
            }),
            sequence: 2,
        });

        // Process with profile
        let profile = ActionLogProfile::with_default_config();
        let events = event_log.events();
        let view_data = profile.process(events);

        // Verify WAIT action is excluded
        assert_eq!(view_data.visible_count, 0, "Should have 0 visible actions");
        assert_eq!(view_data.total_count, 1, "Should have 1 total action (WAIT)");

        println!("✓ PASS: ActionLogProfile correctly excludes WAIT actions");
    }

    #[test]
    fn test_action_log_profile_includes_workflows() {
        println!("\n[TEST] ActionLogProfile includes workflows");

        let mut event_log = EventLog::new();

        // Add workflow_started event
        event_log.add_event(RawEvent {
            id: "test-wf-1".to_string(),
            event_type: "workflow_started".to_string(),
            timestamp: 1234567890.0,
            data: json!({
                "node": {
                    "id": "workflow-1",
                    "node_type": "workflow",
                    "name": "Test Workflow",
                    "timestamp": 1234567890.0,
                    "status": "running",
                    "metadata": {"is_inline": false}
                }
            }),
            sequence: 1,
        });

        event_log.add_event(RawEvent {
            id: "test-wf-2".to_string(),
            event_type: "workflow_completed".to_string(),
            timestamp: 1234567895.0,
            data: json!({
                "node": {
                    "id": "workflow-1",
                    "node_type": "workflow",
                    "name": "Test Workflow",
                    "timestamp": 1234567895.0,
                    "status": "success",
                    "metadata": {}
                }
            }),
            sequence: 2,
        });

        // Process with profile
        let profile = ActionLogProfile::with_default_config();
        let events = event_log.events();
        let view_data = profile.process(events);

        // Verify workflow appears
        assert!(view_data.visible_count > 0, "Workflows should be visible");
        assert!(view_data.actions.len() > 0, "Should return workflow entries");

        println!("✓ PASS: ActionLogProfile includes workflows");
    }
}
