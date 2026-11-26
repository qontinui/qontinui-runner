# Unified Data Architecture Test Suite

This directory contains comprehensive unit tests for the unified data architecture components in the qontinui-runner python-bridge.

## Test Coverage

The test suite includes **111 tests** across these 3 main test modules (the whole suite collects more):

### 1. test_action_execution_record.py (36 tests)

Tests for the immutable `ActionExecutionRecord` dataclass:

- **Record Creation** (2 tests)
  - Minimal fields
  - All fields populated

- **Immutability** (6 tests)
  - Cannot modify action_id, action_type, success, active_states
  - Config and metadata dict defensive copying

- **Computed Properties** (13 tests)
  - duration_seconds calculation
  - state_changed detection
  - states_activated/states_deactivated sets
  - Simultaneous activation and deactivation

- **Tree Event Conversion** (4 tests)
  - Success, failed, and running states
  - State context serialization

- **Runtime Data Formatting** (7 tests)
  - TYPE actions (typed_text)
  - FIND actions (match_summary)
  - CLICK actions (clicked_location, button, target_type)
  - GO_TO_STATE actions (transition_data)
  - IF actions (condition_result, branch_taken)
  - Combined runtime data

- **State Change Detection** (4 tests)
  - Empty to populated states
  - Populated to empty states
  - Complex state sets
  - Partial state changes

### 2. test_unified_data_collector.py (35 tests)

Tests for the `UnifiedDataCollector` service:

- **Initialization** (3 tests)
  - With state_memory and screenshot_service
  - Without services

- **Action Lifecycle** (4 tests)
  - start_action captures initial state
  - Buffer clearing
  - Nested hierarchy
  - Without state memory

- **Runtime Data Recording** (14 tests)
  - record_text_typed()
  - record_match_result() with/without screenshots
  - record_click()
  - record_transition()
  - record_condition()

- **Record Creation** (9 tests)
  - Basic record creation
  - Immutability verification
  - With typed text, clicks, matches
  - Buffer clearing
  - State change capture
  - Duration calculation
  - Error handling

- **Screenshot Integration** (2 tests)
  - Screenshot references in records
  - Debug visual references in records

- **Thread Safety** (3 tests)
  - Concurrent text recording
  - Concurrent click recording
  - Sequential action creation

### 3. test_screenshot_service.py (40 tests)

Tests for the `ScreenshotService`:

- **Initialization** (3 tests)
  - Directory creation
  - Disabled mode
  - Existing directories

- **Screenshot Storage** (6 tests)
  - Basic storage
  - Metadata sidecar creation
  - Sequential numbering
  - Disabled mode returns None
  - Empty metadata/states

- **Debug Visual Storage** (5 tests)
  - Basic storage
  - Metadata creation
  - Filename sanitization
  - Sequential numbering
  - Disabled mode

- **Screenshot Retrieval** (4 tests)
  - By relative path
  - By absolute path
  - Missing file handling
  - Disabled mode

- **Cleanup** (7 tests)
  - Retention policy
  - Metadata deletion
  - Edge cases (no screenshots, fewer than limit)
  - Debug visual cleanup
  - Delete all (keep_last_n=0)

- **Filename Sanitization** (5 tests)
  - Special character removal
  - Space replacement
  - Consecutive hyphen removal
  - Leading/trailing hyphen removal
  - Length limiting

- **Sequential Numbering** (5 tests)
  - Screenshot number calculation
  - Debug visual number calculation
  - Gap handling

- **Disabled Mode** (5 tests)
  - All methods return None or 0
  - No filesystem operations

## Test Infrastructure

### conftest.py

Provides shared fixtures and utilities:

- **MockStateMemory**: Simulates state tracking for testing
- **Fixtures**:
  - `mock_state_memory`: Empty state memory
  - `mock_state_memory_with_states`: Pre-populated with ["Login", "MainMenu"]
  - `temp_storage_dir`: Temporary directory for file operations
  - `sample_action_record`: Typical successful action
  - `sample_failed_action_record`: Failed action
  - `sample_running_action_record`: Incomplete action
  - `action_record_builder`: Function to create custom records
  - `mock_screenshot_service`: Mock service with predefined responses and a real `storage_dir`
  - `sample_png_bytes`: Minimal valid PNG for testing

## Running Tests

### Run all tests

```bash
cd "$QONTINUI_ROOT"/qontinui-runner/python-bridge   # or wherever you cloned it
pytest tests/
```

### Run specific test file

```bash
pytest tests/test_action_execution_record.py
```

### Run specific test class

```bash
pytest tests/test_unified_data_collector.py::TestCreateRecord
```

### Run specific test

```bash
pytest tests/test_screenshot_service.py::TestStoreScreenshot::test_store_screenshot_basic
```

### Run with coverage

```bash
pytest tests/ --cov=models --cov=services --cov-report=html
```

### Run with verbose output

```bash
pytest tests/ -v
```

### Run tests matching pattern

```bash
pytest tests/ -k "state_change"
```

## Test Organization

Tests follow pytest best practices:

1. **Clear test names**: Test names describe what is being tested
2. **Arrange-Act-Assert**: Tests follow AAA pattern
3. **One assertion per concept**: Tests focus on single behaviors
4. **Isolated tests**: Tests don't depend on each other
5. **Fixtures**: Shared setup via pytest fixtures
6. **Mocking**: External dependencies are mocked

## Test Categories

### Unit Tests

All tests in this suite are unit tests that:

- Test single components in isolation
- Use mocks for dependencies
- Run quickly (< 1 second each)
- Don't require external services

### Integration Tests

A separate `test_integration.py` file contains integration tests that verify:

- Component interaction
- End-to-end workflows
- Multiple components working together

## Coverage Goals

Target coverage: **95%+** for all modules:

- `models/action_execution_record.py`
- `services/unified_data_collector.py`
- `services/screenshot_service.py`

## Adding New Tests

When adding new tests:

1. **Use existing fixtures**: Leverage conftest.py fixtures
2. **Follow naming convention**: `test_<what_is_being_tested>`
3. **Group related tests**: Use test classes for organization
4. **Document edge cases**: Include docstrings for complex tests
5. **Test both success and failure**: Cover happy path and error cases
6. **Verify immutability**: Test that frozen dataclasses can't be modified
7. **Mock external dependencies**: Use Mock for I/O operations

## Dependencies

Required packages (from pyproject.toml):

- pytest >= 7.4.3
- pytest-cov (for coverage reporting)
- pytest-asyncio (if async tests are added)

## Continuous Integration

These tests are designed to run in CI/CD pipelines:

- Fast execution (all tests complete in < 5 seconds)
- No external dependencies
- Deterministic (no random behavior)
- Clean up temporary files

## Troubleshooting

### Import errors

Ensure you're running pytest from the python-bridge directory:

```bash
cd "$QONTINUI_ROOT"/qontinui-runner/python-bridge   # or wherever you cloned it
pytest tests/
```

### Fixture not found

Check that conftest.py is in the tests directory and is being loaded.

### Test collection errors

Run with verbose collection to see what's being discovered:

```bash
pytest tests/ --collect-only -v
```

## Future Enhancements

Potential additions to the test suite:

1. **Performance tests**: Benchmark critical operations
2. **Property-based tests**: Use Hypothesis for edge case generation
3. **Mutation testing**: Verify test quality with mutation testing
4. **Async tests**: If async operations are added to the codebase
5. **Parametrized tests**: Use pytest.mark.parametrize for test variations
6. **Stress tests**: Test with large numbers of records

## Contact

For questions about the test suite, see the main project documentation or contact the development team.
