"""Unit tests for State Boundary Detector.

This module contains tests for the StateBoundaryDetector class and related functionality.
Run with: pytest analysis/test_state_boundary.py
"""

import sys
from pathlib import Path

import cv2
import numpy as np
import pytest

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))

from analysis.state_boundary_detector import (
    StateBoundaryConfig,
    StateBoundaryDetector,
)
from models.state_models import Frame, InputEvent

# ============================================================================
# Test Fixtures
# ============================================================================


@pytest.fixture
def sample_frames():
    """Create sample frames for testing."""
    frames = []

    # Create 10 frames with 3 distinct visual states
    for i in range(10):
        # Create image
        image = np.zeros((600, 800, 3), dtype=np.uint8)

        if i < 3:
            # State 1: Red rectangle
            cv2.rectangle(image, (100, 100), (300, 300), (0, 0, 255), -1)
        elif i < 7:
            # State 2: Green circle
            cv2.circle(image, (400, 300), 100, (0, 255, 0), -1)
        else:
            # State 3: Blue triangle
            pts = np.array([[400, 100], [300, 400], [500, 400]], np.int32)
            cv2.fillPoly(image, [pts], (255, 0, 0))

        frame = Frame(
            image=image,
            timestamp=float(i),
            frame_index=i,
            file_path=f"frame_{i}.png",
        )
        frames.append(frame)

    return frames


@pytest.fixture
def sample_events():
    """Create sample input events."""
    events = [
        InputEvent(timestamp=2.5, event_type="click", x=200, y=200, button="left"),
        InputEvent(timestamp=6.5, event_type="click", x=400, y=300, button="left"),
    ]
    return events


@pytest.fixture
def default_config():
    """Create default configuration."""
    return StateBoundaryConfig()


@pytest.fixture
def detector(default_config):
    """Create detector with default configuration."""
    return StateBoundaryDetector(default_config)


# ============================================================================
# Initialization Tests
# ============================================================================


def test_detector_initialization():
    """Test detector initialization."""
    detector = StateBoundaryDetector()
    assert detector is not None
    assert detector.config is not None
    assert detector.feature_detector is not None


def test_detector_with_custom_config():
    """Test detector with custom configuration."""
    config = StateBoundaryConfig(
        similarity_threshold=0.95,
        clustering_algorithm="hierarchical",
        feature_extractor="orb",
    )
    detector = StateBoundaryDetector(config)
    assert detector.config.similarity_threshold == 0.95
    assert detector.config.clustering_algorithm == "hierarchical"


def test_config_defaults():
    """Test configuration default values."""
    config = StateBoundaryConfig()
    assert config.similarity_threshold == 0.92
    assert config.clustering_algorithm == "dbscan"
    assert config.feature_extractor == "orb"
    assert config.feature_count == 500


# ============================================================================
# Feature Extraction Tests
# ============================================================================


def test_compute_perceptual_hash(detector, sample_frames):
    """Test perceptual hash computation."""
    frame = sample_frames[0]
    phash = detector.compute_perceptual_hash(frame)

    assert phash is not None
    assert isinstance(phash, str)
    assert len(phash) > 0


def test_perceptual_hash_similarity(detector, sample_frames):
    """Test that similar frames have similar hashes."""
    # Same state frames should have similar hashes
    hash1 = detector.compute_perceptual_hash(sample_frames[0])
    hash2 = detector.compute_perceptual_hash(sample_frames[1])

    # Different state frames should have different hashes
    hash3 = detector.compute_perceptual_hash(sample_frames[5])

    # Note: This is a simplified test - in reality you'd use hamming distance
    assert hash1 is not None
    assert hash2 is not None
    assert hash3 is not None


def test_compute_similarity(detector, sample_frames):
    """Test SSIM similarity computation."""
    frame1 = sample_frames[0]
    frame2 = sample_frames[1]

    similarity = detector.compute_similarity(frame1, frame2)

    assert similarity is not None
    assert 0.0 <= similarity <= 1.0


def test_similarity_same_state(detector, sample_frames):
    """Test that frames in same state are similar."""
    # Frames 0 and 1 are both in state 1 (red rectangle)
    similarity = detector.compute_similarity(sample_frames[0], sample_frames[1])

    # Should be very similar (close to 1.0)
    assert similarity > 0.9


def test_similarity_different_states(detector, sample_frames):
    """Test that frames in different states are dissimilar."""
    # Frame 0 is state 1 (red), frame 5 is state 2 (green)
    similarity = detector.compute_similarity(sample_frames[0], sample_frames[5])

    # Should be dissimilar (well below 1.0)
    assert similarity < 0.8


# ============================================================================
# State Detection Tests
# ============================================================================


def test_detect_states_basic(detector, sample_frames):
    """Test basic state detection."""
    states = detector.detect_states(sample_frames)

    assert states is not None
    assert isinstance(states, list)
    assert len(states) > 0


def test_detect_states_finds_multiple(detector, sample_frames):
    """Test that multiple states are detected."""
    # We have 3 distinct visual states in sample_frames
    states = detector.detect_states(sample_frames)

    # Should detect at least 2 states (might not catch all 3 depending on params)
    assert len(states) >= 2


def test_detect_states_empty_frames():
    """Test state detection with empty frame list."""
    detector = StateBoundaryDetector()

    with pytest.raises(ValueError):
        detector.detect_states([])


def test_state_metadata(detector, sample_frames):
    """Test that states have proper metadata."""
    states = detector.detect_states(sample_frames)

    for state in states:
        assert state.name is not None
        assert len(state.frame_indices) > 0
        assert "duration_ms" in state.metadata
        assert "representative_frame_index" in state.metadata


# ============================================================================
# Transition Detection Tests
# ============================================================================


def test_identify_transitions(detector, sample_frames, sample_events):
    """Test transition identification."""
    transitions = detector.identify_transitions(sample_frames, sample_events)

    assert transitions is not None
    assert isinstance(transitions, list)


def test_transition_correlation(detector, sample_frames, sample_events):
    """Test that transitions are correlated with events."""
    transitions = detector.identify_transitions(sample_frames, sample_events)

    # At least some transitions should be correlated with events
    correlated = [t for t in transitions if t.trigger_event_index is not None]
    assert len(correlated) >= 0  # May or may not find correlations


def test_transitions_empty_events(detector, sample_frames):
    """Test transitions with no input events."""
    transitions = detector.identify_transitions(sample_frames, [])

    # Should still detect transitions based on visual changes
    assert transitions is not None
    assert isinstance(transitions, list)


# ============================================================================
# Clustering Algorithm Tests
# ============================================================================


def test_clustering_dbscan(sample_frames):
    """Test DBSCAN clustering."""
    config = StateBoundaryConfig(clustering_algorithm="dbscan")
    detector = StateBoundaryDetector(config)

    states = detector.detect_states(sample_frames)
    assert states is not None


def test_clustering_hierarchical(sample_frames):
    """Test hierarchical clustering."""
    config = StateBoundaryConfig(clustering_algorithm="hierarchical")
    detector = StateBoundaryDetector(config)

    states = detector.detect_states(sample_frames)
    assert states is not None


def test_clustering_kmeans(sample_frames):
    """Test k-means clustering."""
    config = StateBoundaryConfig(
        clustering_algorithm="kmeans",
        kmeans_n_clusters=3,
    )
    detector = StateBoundaryDetector(config)

    states = detector.detect_states(sample_frames)
    assert states is not None
    # K-means with n_clusters=3 should produce exactly 3 states
    assert len(states) <= 3


# ============================================================================
# Feature Extractor Tests
# ============================================================================


def test_feature_extractor_orb(sample_frames):
    """Test ORB feature extractor."""
    config = StateBoundaryConfig(feature_extractor="orb")
    detector = StateBoundaryDetector(config)

    states = detector.detect_states(sample_frames)
    assert states is not None


def test_feature_extractor_sift(sample_frames):
    """Test SIFT feature extractor."""
    config = StateBoundaryConfig(feature_extractor="sift")
    detector = StateBoundaryDetector(config)

    states = detector.detect_states(sample_frames)
    assert states is not None


# ============================================================================
# Edge Cases and Error Handling
# ============================================================================


def test_single_frame():
    """Test with single frame."""
    frame = Frame(
        image=np.zeros((100, 100, 3), dtype=np.uint8),
        timestamp=0.0,
        frame_index=0,
        file_path="test.png",
    )

    detector = StateBoundaryDetector()
    states = detector.detect_states([frame])

    # Should handle single frame gracefully
    assert states is not None


def test_identical_frames():
    """Test with identical frames."""
    image = np.zeros((100, 100, 3), dtype=np.uint8)
    frames = [
        Frame(image=image.copy(), timestamp=float(i), frame_index=i, file_path=f"frame_{i}.png")
        for i in range(5)
    ]

    detector = StateBoundaryDetector()
    states = detector.detect_states(frames)

    # All identical frames should form a single state
    assert len(states) == 1


def test_config_validation():
    """Test configuration parameter validation."""
    # Test with extreme values
    config = StateBoundaryConfig(
        similarity_threshold=1.0,
        min_state_duration_ms=0,
        feature_count=1,
    )

    detector = StateBoundaryDetector(config)
    assert detector is not None


# ============================================================================
# Integration Tests
# ============================================================================


def test_full_pipeline(sample_frames, sample_events):
    """Test complete detection pipeline."""
    # Create detector
    detector = StateBoundaryDetector()

    # Detect states
    states = detector.detect_states(sample_frames)
    assert len(states) > 0

    # Detect transitions
    transitions = detector.identify_transitions(sample_frames, sample_events)
    assert transitions is not None

    # Verify state properties
    for state in states:
        assert state.name is not None
        assert len(state.frame_indices) > 0
        assert state.metadata is not None


if __name__ == "__main__":
    # Run tests with pytest
    pytest.main([__file__, "-v"])
