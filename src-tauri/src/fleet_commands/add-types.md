# Add Missing Type Annotations

Systematically add type annotations to Python code that lacks them.

## Instructions

**IMPORTANT**: This command modifies code. Work incrementally and verify changes don't break functionality.

---

### Phase 1: Analyze Type Coverage

1. **Run type coverage analysis**:
   ```bash
   cd $PWD/qontinui-devtools
   poetry run qontinui-devtools types coverage /path/to/project
   ```

2. **Run mypy to identify untyped code**:
   ```bash
   cd /path/to/project
   poetry run mypy --package <package> --warn-return-any --warn-unused-ignores 2>&1
   ```

3. **Create a prioritized list**:
   - Public API functions (highest priority)
   - Class methods
   - Module-level functions
   - Internal helpers (lowest priority)

---

### Phase 2: Add Type Annotations

For each file needing types, apply these patterns:

#### Function Parameters and Returns
```python
# Before:
def process(data, config=None):
    return data.upper()

# After:
def process(data: str, config: dict[str, Any] | None = None) -> str:
    return data.upper()
```

#### Class Attributes
```python
# Before:
class User:
    def __init__(self, name, age):
        self.name = name
        self.age = age

# After:
class User:
    name: str
    age: int

    def __init__(self, name: str, age: int) -> None:
        self.name = name
        self.age = age
```

#### Collections
```python
# Before:
def get_users():
    return [{"name": "Alice"}]

# After:
def get_users() -> list[dict[str, str]]:
    return [{"name": "Alice"}]
```

#### Optional and Union Types
```python
# Before:
def find_user(id):
    return None  # or User

# After:
def find_user(id: int) -> User | None:
    return None  # or User
```

---

### Phase 3: Handle Complex Cases

#### Callbacks and Callables
```python
from collections.abc import Callable

def on_event(callback: Callable[[str, int], None]) -> None:
    callback("event", 42)
```

#### Generic Types
```python
from typing import TypeVar

T = TypeVar("T")

def first(items: list[T]) -> T | None:
    return items[0] if items else None
```

#### Type Aliases
```python
from typing import TypeAlias

UserId: TypeAlias = int
UserMap: TypeAlias = dict[UserId, "User"]
```

#### Forward References
```python
from __future__ import annotations

class Node:
    def add_child(self, child: Node) -> None:  # Works without quotes
        pass
```

---

### Phase 4: Common Import Additions

Add these imports as needed:

```python
from __future__ import annotations  # At top of file for forward refs

from typing import Any, TypeVar, TypeAlias
from collections.abc import Callable, Iterator, Sequence, Mapping
```

---

### Phase 5: Verify Changes

After adding types to each file:

1. **Run mypy**:
   ```bash
   poetry run mypy --package <package>
   ```

2. **Fix any new errors** introduced by the type annotations

3. **Run tests** to ensure functionality unchanged:
   ```bash
   poetry run pytest
   ```

---

### Phase 6: Iterative Processing

Use Task agents to parallelize work across multiple files:

1. **Batch files by module** (5-10 files per batch)
2. **Launch parallel Task agents** to add types
3. **Run mypy after each batch** to catch issues early
4. **Continue until all files are typed**

---

### Notes

- Prefer modern syntax: `list[str]` over `List[str]`, `str | None` over `Optional[str]`
- Use `Any` sparingly - prefer specific types
- Add `# type: ignore[<code>]` only when truly necessary
- Don't type private helper functions unless they're complex
- Focus on public API surfaces first
