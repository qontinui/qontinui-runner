# Generate Unit Tests

Systematically create unit tests for untested code.

## Instructions

**IMPORTANT**: This command creates new test files. It does NOT modify existing source code.

---

### Phase 1: Analyze Test Coverage

1. **Identify existing test structure**:
   ```bash
   # Find existing tests
   find /path/to/project -name "test_*.py" -o -name "*_test.py"

   # Check test directory structure
   ls -la /path/to/project/tests/
   ```

2. **Run coverage analysis** (if pytest-cov available):
   ```bash
   cd /path/to/project
   poetry run pytest --cov=<package> --cov-report=term-missing
   ```

3. **Identify untested modules**:
   - List all source files
   - Cross-reference with existing tests
   - Prioritize by importance (core logic > utilities > helpers)

---

### Phase 2: Understand Testing Patterns

Before writing tests, analyze existing test patterns:

1. **Read 2-3 existing test files** to understand:
   - Import conventions
   - Fixture usage
   - Mocking patterns
   - Assertion style
   - Naming conventions

2. **Identify test dependencies**:
   - pytest plugins in use
   - Custom fixtures in conftest.py
   - Mock/patch patterns

---

### Phase 3: Generate Tests

For each untested module, create tests following this structure:

#### Test File Template
```python
"""Tests for {module_name}."""

from __future__ import annotations

import pytest
from unittest.mock import Mock, patch, MagicMock

from {package}.{module} import {functions_and_classes}


class Test{ClassName}:
    """Tests for {ClassName}."""

    def test_init(self) -> None:
        """Test initialization."""
        obj = ClassName()
        assert obj is not None

    def test_method_success(self) -> None:
        """Test method with valid input."""
        obj = ClassName()
        result = obj.method("valid_input")
        assert result == expected_value

    def test_method_edge_case(self) -> None:
        """Test method with edge case."""
        obj = ClassName()
        result = obj.method("")
        assert result == expected_edge_result

    def test_method_error(self) -> None:
        """Test method raises on invalid input."""
        obj = ClassName()
        with pytest.raises(ValueError):
            obj.method(None)


class TestFunctionName:
    """Tests for function_name."""

    def test_basic(self) -> None:
        """Test basic functionality."""
        result = function_name("input")
        assert result == "expected"

    @pytest.mark.parametrize("input_val,expected", [
        ("a", "A"),
        ("b", "B"),
        ("", ""),
    ])
    def test_parametrized(self, input_val: str, expected: str) -> None:
        """Test with multiple inputs."""
        assert function_name(input_val) == expected
```

---

### Phase 4: Test Categories

Generate tests for each category:

#### 1. Happy Path Tests
- Normal inputs produce expected outputs
- Standard use cases work correctly

#### 2. Edge Case Tests
- Empty inputs
- Boundary values
- Maximum/minimum values
- None/null handling

#### 3. Error Case Tests
- Invalid inputs raise appropriate exceptions
- Error messages are helpful
- Cleanup happens on failure

#### 4. Integration Tests (if applicable)
- Components work together
- External dependencies are mocked

---

### Phase 5: Mocking Patterns

#### Mock External Dependencies
```python
@patch("module.external_api_call")
def test_with_mock(self, mock_api: Mock) -> None:
    mock_api.return_value = {"status": "ok"}
    result = function_that_calls_api()
    assert result.status == "ok"
    mock_api.assert_called_once()
```

#### Mock Database
```python
@pytest.fixture
def mock_db() -> Mock:
    db = Mock()
    db.query.return_value = [{"id": 1, "name": "Test"}]
    return db

def test_with_db(self, mock_db: Mock) -> None:
    result = get_users(mock_db)
    assert len(result) == 1
```

#### Mock File System
```python
def test_file_read(self, tmp_path: Path) -> None:
    test_file = tmp_path / "test.txt"
    test_file.write_text("content")
    result = read_file(test_file)
    assert result == "content"
```

---

### Phase 6: Async Tests

For async code:

```python
import pytest

@pytest.mark.asyncio
async def test_async_function() -> None:
    result = await async_function()
    assert result == expected
```

---

### Phase 7: Verify Tests

After creating tests:

1. **Run the new tests**:
   ```bash
   poetry run pytest tests/test_new_module.py -v
   ```

2. **Check coverage improvement**:
   ```bash
   poetry run pytest --cov=<package> --cov-report=term-missing
   ```

3. **Fix any failures** before moving to next module

---

### Phase 8: Parallel Processing

Use Task agents to parallelize:

1. **Group modules** by directory (3-5 modules per batch)
2. **Launch parallel agents** to generate tests
3. **Run pytest after each batch**
4. **Continue until target coverage reached**

---

### Notes

- Match existing test style in the project
- Don't test private methods directly (test through public API)
- Keep tests focused - one concept per test
- Use descriptive test names that explain what's being tested
- Avoid testing framework/library code - focus on application logic
- Prefer real objects over mocks when practical
