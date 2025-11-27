"""Example usage of the Transition Analyzer service.

This script demonstrates how to use the TransitionAnalyzer and AutoTransitionBuilder
to automatically identify state transitions from captured video sessions.
"""

import logging
from typing import List
import numpy as np

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)

from analysis.transition_analyzer import (
    TransitionAnalyzer,
    AutoTransitionBuilder,
    CaptureSession,
)
from models.state_models import DetectedState, StateImage, Frame, InputEvent


def create_sample_state_images() -> List[StateImage]:
    """Create sample StateImages for demonstration."""
    # Create dummy images (in real usage, these would be actual screenshots)
    dummy_image = np.zeros((50, 50, 3), dtype=np.uint8)

    return [
        StateImage(
            name="login_button",
            image=dummy_image,
            bbox=(100, 200, 150, 40),
            position_type="fixed",
            position=(100, 200),
            similarity_threshold=0.9,
        ),
        StateImage(
            name="username_field",
            image=dummy_image,
            bbox=(100, 100, 200, 30),
            position_type="fixed",
            position=(100, 100),
            similarity_threshold=0.85,
        ),
        StateImage(
            name="dashboard_logo",
            image=dummy_image,
            bbox=(50, 50, 100, 100),
            position_type="fixed",
            position=(50, 50),
            similarity_threshold=0.95,
        ),
    ]


def create_sample_states() -> List[DetectedState]:
    """Create sample detected states for demonstration."""
    state_images = create_sample_state_images()

    states = [
        DetectedState(
            name="LoginScreen",
            description="Login screen with username/password fields",
            state_images=[state_images[0], state_images[1]],  # login_button, username_field
            start_frame_index=0,
            end_frame_index=29,
            frame_indices=list(range(0, 30)),
        ),
        DetectedState(
            name="Dashboard",
            description="Main dashboard after login",
            state_images=[state_images[2]],  # dashboard_logo
            start_frame_index=30,
            end_frame_index=59,
            frame_indices=list(range(30, 60)),
        ),
    ]

    return states


def create_sample_events() -> List[InputEvent]:
    """Create sample input events for demonstration."""
    return [
        InputEvent(
            timestamp=0.5,
            frame_number=15,
            event_type="mouse_click",
            x=175,  # Near login_button (100, 200, 150, 40)
            y=220,
            button="left",
        ),
        InputEvent(
            timestamp=0.95,
            frame_number=28,
            event_type="mouse_click",
            x=175,
            y=220,
            button="left",
        ),
    ]


def create_sample_frames() -> List[Frame]:
    """Create sample frames for demonstration."""
    frames = []
    dummy_image = np.zeros((600, 800, 3), dtype=np.uint8)

    for i in range(60):
        frame = Frame(
            image=dummy_image,
            timestamp=i / 30.0,  # 30 FPS
            frame_index=i,
        )
        frames.append(frame)

    return frames


def example_basic_analysis():
    """Example 1: Basic transition analysis."""
    print("\n" + "=" * 80)
    print("Example 1: Basic Transition Analysis")
    print("=" * 80 + "\n")

    # Create analyzer
    analyzer = TransitionAnalyzer(
        event_correlation_window_ms=1000,
        click_proximity_threshold=50,
        min_visual_change_score=0.1,
    )

    # Prepare sample data
    states = create_sample_states()
    events = create_sample_events()
    frames = create_sample_frames()

    print(f"Input data:")
    print(f"  States: {len(states)}")
    print(f"  Events: {len(events)}")
    print(f"  Frames: {len(frames)}")
    print()

    # Analyze transitions
    transitions = analyzer.analyze_transitions(states, events, frames)

    # Display results
    print(f"Detected {len(transitions)} transitions:\n")

    for i, transition in enumerate(transitions, 1):
        print(f"Transition {i}:")
        print(f"  ID: {transition.id}")
        print(f"  Source: {transition.get_primary_source_state()}")
        print(f"  Target: {transition.get_primary_target_state()}")
        print(f"  Action: {transition.action_type}")
        print(f"  Action Target: {transition.action_target or 'Not identified'}")
        print(f"  Action Location: {transition.action_location}")
        print(f"  Recognition Images: {transition.recognition_images}")
        print(f"  Frames: {transition.frame_before} → {transition.frame_after}")
        print(f"  Duration: {transition.duration_ms}ms")
        print(f"  Simple Transition: {transition.is_simple_transition}")
        print()


def example_auto_builder():
    """Example 2: Using AutoTransitionBuilder."""
    print("\n" + "=" * 80)
    print("Example 2: Auto Transition Builder")
    print("=" * 80 + "\n")

    # Create capture session
    session = CaptureSession(
        frames=create_sample_frames(),
        states=create_sample_states(),
        events=create_sample_events(),
        fps=30,
        metadata={
            "session_id": "example_session_1",
            "application": "DemoApp",
            "capture_date": "2024-01-01",
        }
    )

    print(f"Capture session:")
    print(f"  FPS: {session.fps}")
    print(f"  States: {len(session.states)}")
    print(f"  Events: {len(session.events)}")
    print(f"  Metadata: {session.metadata}")
    print()

    # Build transitions
    builder = AutoTransitionBuilder()
    transitions = builder.build_from_capture(session)

    # Display results with suggested names
    print(f"Generated {len(transitions)} transitions:\n")

    for transition in transitions:
        suggested_name = transition.metadata.get("suggested_name", "Unnamed")
        print(f"Transition: {suggested_name}")
        print(f"  ID: {transition.id}")
        print(f"  Has Action Target: {transition.has_action_target}")
        print(f"  Recognition Confidence: {transition.recognition_confidence}")
        print(f"  Visual Change Score: {transition.metadata.get('visual_change_score', 'N/A')}")
        print()


def example_export_to_json():
    """Example 3: Export transitions to JSON."""
    print("\n" + "=" * 80)
    print("Example 3: Export to JSON")
    print("=" * 80 + "\n")

    # Build transitions
    analyzer = TransitionAnalyzer()
    transitions = analyzer.analyze_transitions(
        create_sample_states(),
        create_sample_events(),
        create_sample_frames(),
    )

    # Export to dictionary
    transition_data = [t.to_dict() for t in transitions]

    # Display JSON structure
    if transition_data:
        import json
        print("Transition JSON structure:")
        print(json.dumps(transition_data[0], indent=2, default=str))
        print()
        print(f"Total transitions: {len(transition_data)}")


def example_custom_configuration():
    """Example 4: Custom analyzer configuration."""
    print("\n" + "=" * 80)
    print("Example 4: Custom Configuration")
    print("=" * 80 + "\n")

    # Create analyzer with tight timing requirements
    strict_analyzer = TransitionAnalyzer(
        event_correlation_window_ms=200,   # Very tight window
        click_proximity_threshold=25,       # Precise click matching
        min_visual_change_score=0.3,        # Higher change threshold
    )

    print("Strict analyzer configuration:")
    print(f"  Correlation window: 200ms")
    print(f"  Click proximity: 25px")
    print(f"  Min visual change: 0.3")
    print()

    transitions = strict_analyzer.analyze_transitions(
        create_sample_states(),
        create_sample_events(),
        create_sample_frames(),
    )

    print(f"Detected {len(transitions)} transitions with strict settings\n")

    # Create analyzer with relaxed requirements
    relaxed_analyzer = TransitionAnalyzer(
        event_correlation_window_ms=2000,  # Wide window
        click_proximity_threshold=100,      # Large click area
        min_visual_change_score=0.05,       # Low change threshold
    )

    print("Relaxed analyzer configuration:")
    print(f"  Correlation window: 2000ms")
    print(f"  Click proximity: 100px")
    print(f"  Min visual change: 0.05")
    print()

    transitions = relaxed_analyzer.analyze_transitions(
        create_sample_states(),
        create_sample_events(),
        create_sample_frames(),
    )

    print(f"Detected {len(transitions)} transitions with relaxed settings\n")


def main():
    """Run all examples."""
    print("\n" + "=" * 80)
    print("TRANSITION ANALYZER EXAMPLES")
    print("=" * 80)

    try:
        example_basic_analysis()
        example_auto_builder()
        example_export_to_json()
        example_custom_configuration()

        print("\n" + "=" * 80)
        print("All examples completed successfully!")
        print("=" * 80 + "\n")

    except Exception as e:
        print(f"\nError running examples: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    main()
