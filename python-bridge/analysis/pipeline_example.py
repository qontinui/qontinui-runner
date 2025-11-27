"""Example usage of the Analysis Pipeline Orchestrator.

This script demonstrates how to use the AnalysisPipeline to process
captured video sessions and extract states, StateImages, and transitions.
"""

import logging
import sys
from pathlib import Path

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from analysis.pipeline import (
    AnalysisPipeline,
    PipelineConfig,
    CaptureSession,
    load_session_from_video,
    save_analysis_result,
)
from models import Frame, InputEvent
import numpy as np

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='[%(asctime)s] %(levelname)s - %(name)s - %(message)s'
)
logger = logging.getLogger(__name__)


def create_mock_session() -> CaptureSession:
    """Create a mock capture session for demonstration.

    In a real scenario, you would use load_session_from_video() to load
    actual video and events data.

    Returns:
        Mock CaptureSession with synthetic data
    """
    logger.info("Creating mock capture session...")

    frames = []
    events = []

    # Create some mock frames (in reality, these would be actual screenshots)
    for i in range(100):
        # Create a simple gradient image
        img = np.zeros((800, 600, 3), dtype=np.uint8)
        # Change the image every 20 frames to simulate state changes
        state_num = i // 20
        img[:, :] = (50 * state_num, 100, 150)

        frame = Frame(
            image=img,
            timestamp=i / 30.0,  # 30 FPS
            frame_index=i,
        )
        frames.append(frame)

    # Create some mock click events
    click_times = [0.5, 1.5, 2.5, 3.5]  # Clicks at different times
    for i, click_time in enumerate(click_times):
        event = InputEvent(
            timestamp=click_time,
            event_type="mouse_click",
            x=300 + i * 50,
            y=400 + i * 20,
            button="left",
        )
        events.append(event)

    session = CaptureSession(
        session_id="demo_session_001",
        video_path="mock_video.mp4",
        events_path="mock_events.json",
        fps=30,
        frames=frames,
        events=events,
        metadata={
            "description": "Mock session for demonstration",
            "app_name": "Demo Application",
        }
    )

    logger.info("Created mock session: %d frames, %d events", len(frames), len(events))
    return session


def example_basic_usage():
    """Example 1: Basic pipeline usage with default configuration."""
    logger.info("=" * 80)
    logger.info("EXAMPLE 1: Basic Pipeline Usage")
    logger.info("=" * 80)

    # Create mock session
    session = create_mock_session()

    # Create pipeline with default configuration
    pipeline = AnalysisPipeline()

    # Run analysis
    result = pipeline.analyze_session(session)

    # Print results
    logger.info("\n" + "=" * 80)
    logger.info("ANALYSIS RESULTS")
    logger.info("=" * 80)
    logger.info("Success: %s", result.success)
    logger.info("States detected: %d", len(result.states))
    logger.info("Transitions detected: %d", len(result.transitions))
    logger.info("Processing duration: %.2fs", result.processing_log.total_duration)

    # Print processing steps
    logger.info("\nProcessing Steps:")
    for step in result.processing_log.steps:
        logger.info(
            "  - %s: %d inputs -> %d outputs (%.2fms) [%s]",
            step.name,
            step.input_count,
            step.output_count,
            step.duration_ms,
            "SUCCESS" if step.success else "FAILED"
        )

    # Print state details
    if result.states:
        logger.info("\nDetected States:")
        for state in result.states:
            logger.info(
                "  - %s: frames %d-%d (%d total), %d StateImages",
                state.name,
                state.start_frame_index,
                state.end_frame_index,
                len(state.frame_indices),
                len(state.state_images)
            )

    # Print transition details
    if result.transitions:
        logger.info("\nDetected Transitions:")
        for transition in result.transitions:
            logger.info(
                "  - %s: %s -> %s (action: %s)",
                transition.id,
                transition.get_primary_source_state(),
                transition.get_primary_target_state(),
                transition.action_type
            )

    return result


def example_custom_configuration():
    """Example 2: Using custom pipeline configuration."""
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 2: Custom Configuration")
    logger.info("=" * 80)

    # Create custom configuration
    config = PipelineConfig(
        # More aggressive state detection
        state_similarity_threshold=0.95,
        min_state_duration_ms=300,
        clustering_algorithm="hierarchical",

        # More aggressive image extraction
        extract_at_clicks=True,
        click_region_padding=30,
        extract_from_contours=True,
        max_contours=100,

        # Wider event correlation window
        event_correlation_window_ms=1500,
    )

    logger.info("Using custom configuration:")
    logger.info("  - state_similarity_threshold: %.2f", config.state_similarity_threshold)
    logger.info("  - clustering_algorithm: %s", config.clustering_algorithm)
    logger.info("  - click_region_padding: %d", config.click_region_padding)

    # Create session and pipeline
    session = create_mock_session()
    pipeline = AnalysisPipeline(config=config)

    # Run analysis
    result = pipeline.analyze_session(session)

    logger.info("\nResults with custom config:")
    logger.info("  - States: %d", len(result.states))
    logger.info("  - Transitions: %d", len(result.transitions))
    logger.info("  - Duration: %.2fs", result.processing_log.total_duration)

    return result


def example_parameter_comparison():
    """Example 3: Compare results with different parameters."""
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 3: Parameter Comparison")
    logger.info("=" * 80)

    session = create_mock_session()

    # Configuration 1: Conservative (higher similarity threshold)
    config1 = PipelineConfig(
        state_similarity_threshold=0.95,
        min_state_duration_ms=1000,
    )

    # Configuration 2: Aggressive (lower similarity threshold)
    config2 = PipelineConfig(
        state_similarity_threshold=0.85,
        min_state_duration_ms=300,
    )

    # Run with both configurations
    logger.info("Running analysis with config 1 (conservative)...")
    pipeline1 = AnalysisPipeline(config=config1)
    result1 = pipeline1.analyze_session(session)

    logger.info("Running analysis with config 2 (aggressive)...")
    pipeline2 = AnalysisPipeline(config=config2)
    result2 = pipeline2.analyze_session(session)

    # Compare results
    comparison = pipeline1.compare_results(result1, result2)

    logger.info("\n" + "=" * 80)
    logger.info("COMPARISON RESULTS")
    logger.info("=" * 80)

    logger.info("\nConfiguration 1 (Conservative):")
    logger.info("  - States: %d", comparison["result1"]["num_states"])
    logger.info("  - Transitions: %d", comparison["result1"]["num_transitions"])
    logger.info("  - StateImages: %d", comparison["result1"]["num_state_images"])
    logger.info("  - Errors: %d", comparison["result1"]["errors"])

    logger.info("\nConfiguration 2 (Aggressive):")
    logger.info("  - States: %d", comparison["result2"]["num_states"])
    logger.info("  - Transitions: %d", comparison["result2"]["num_transitions"])
    logger.info("  - StateImages: %d", comparison["result2"]["num_state_images"])
    logger.info("  - Errors: %d", comparison["result2"]["errors"])

    logger.info("\nDifferences:")
    logger.info("  - States delta: %+d", comparison["differences"]["states_delta"])
    logger.info("  - Transitions delta: %+d", comparison["differences"]["transitions_delta"])
    logger.info("  - StateImages delta: %+d", comparison["differences"]["state_images_delta"])

    logger.info("\nRecommendation: %s", comparison["recommendation"])

    return comparison


def example_incremental_processing():
    """Example 4: Run individual pipeline phases."""
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 4: Incremental Processing")
    logger.info("=" * 80)

    session = create_mock_session()
    pipeline = AnalysisPipeline()

    # Phase 1: State detection only
    logger.info("Running Phase 1: State Detection...")
    states = pipeline.run_phase("state_detection", session)
    logger.info("Detected %d states", len(states))

    # Phase 2: Image extraction
    logger.info("\nRunning Phase 2: Image Extraction...")
    pipeline.run_phase("image_extraction", session, states=states)
    total_images = sum(len(s.state_images) for s in states)
    logger.info("Extracted %d total StateImages", total_images)

    # Phase 3: Transition analysis
    logger.info("\nRunning Phase 3: Transition Analysis...")
    transitions = pipeline.run_phase("transition_analysis", session, states=states)
    logger.info("Detected %d transitions", len(transitions))

    logger.info("\nIncremental processing complete!")
    return states, transitions


def example_rerun_with_new_config():
    """Example 5: Re-run analysis with updated configuration."""
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 5: Re-run with New Configuration")
    logger.info("=" * 80)

    session = create_mock_session()

    # Initial run with default config
    logger.info("Initial run with default configuration...")
    pipeline = AnalysisPipeline()
    result1 = pipeline.analyze_session(session)
    logger.info("Initial results: %d states, %d transitions",
               len(result1.states), len(result1.transitions))

    # Create new configuration
    new_config = PipelineConfig(
        state_similarity_threshold=0.88,
        clustering_algorithm="kmeans",
        extract_at_clicks=True,
        extract_from_contours=True,
    )

    # Re-run with new configuration
    logger.info("\nRe-running with updated configuration...")
    result2 = pipeline.rerun_with_config(session, new_config)
    logger.info("Updated results: %d states, %d transitions",
               len(result2.states), len(result2.transitions))

    # Compare
    comparison = pipeline.compare_results(result1, result2)
    logger.info("\nDifference: %+d states, %+d transitions",
               comparison["differences"]["states_delta"],
               comparison["differences"]["transitions_delta"])

    return result1, result2


def example_save_results():
    """Example 6: Save analysis results to disk."""
    logger.info("\n" + "=" * 80)
    logger.info("EXAMPLE 6: Save Results to Disk")
    logger.info("=" * 80)

    session = create_mock_session()
    pipeline = AnalysisPipeline()
    result = pipeline.analyze_session(session)

    # Save to disk
    output_dir = "/tmp/qontinui_analysis_results"
    result_file = save_analysis_result(result, output_dir)

    logger.info("Results saved to: %s", result_file)
    logger.info("Output directory: %s", output_dir)

    return result_file


def main():
    """Run all examples."""
    logger.info("\n" + "=" * 80)
    logger.info("QONTINUI ANALYSIS PIPELINE - EXAMPLES")
    logger.info("=" * 80)

    try:
        # Run all examples
        example_basic_usage()
        example_custom_configuration()
        example_parameter_comparison()
        example_incremental_processing()
        example_rerun_with_new_config()
        example_save_results()

        logger.info("\n" + "=" * 80)
        logger.info("ALL EXAMPLES COMPLETED SUCCESSFULLY")
        logger.info("=" * 80)

    except Exception as e:
        logger.error("Example failed with error: %s", e, exc_info=True)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
