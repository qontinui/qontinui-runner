# Code Fix - Analyze and Implement Solutions

Run code analysis tools and automatically implement fixes for identified issues.

## Instructions

This command runs analysis tools and then implements fixes for the issues found.

**IMPORTANT**: This command makes code changes. Review the analysis output before confirming fixes.

---

## Phase 1: Run Analysis

First, run the appropriate analysis based on the target code:

### For Python Code
```bash
cd $PWD/qontinui-devtools

# Run comprehensive analysis
poetry run qontinui-devtools analyze /path/to/python/code --report /tmp/analysis.html

# Or run specific analyses
poetry run qontinui-devtools import check /path/to/python/code
poetry run qontinui-devtools quality dead-code /path/to/python/code
poetry run qontinui-devtools security scan /path/to/python/code
```

### For TypeScript/JavaScript Code
```bash
cd $PWD/qontinui-devtools

poetry run qontinui-devtools ts analyze /path/to/ts/code
```

### For Rust Code
```bash
cd $PWD/qontinui-devtools

poetry run qontinui-devtools rust analyze /path/to/rust/src
```

---

## Phase 2: Categorize Issues

After running analysis, categorize issues by type and priority:

### Critical Issues (Fix First)
1. **Circular Dependencies** - Break import cycles
2. **Security Vulnerabilities** - Hardcoded secrets, injection risks
3. **Unsafe Code** (Rust) - Unjustified unsafe blocks

### High Priority Issues
1. **Dead Imports** - Remove unused imports
2. **Dead Code** (confidence > 0.90) - Remove unused functions/classes
3. **God Classes** - Split large classes

### Medium Priority Issues
1. **Missing Type Hints** - Add type annotations
2. **High Complexity** - Refactor complex functions
3. **Type Coverage Gaps** - Improve TypeScript types

---

## Phase 2.5: Peer build hotspot check (best-effort)

Before editing a file, query coord for peer agents' recent build state on
that path. Detects "Agent A's last build is red on `verification/wsm.rs`,
and you're about to edit it" — saves a build cycle and avoids parallel
fixes of the same lint.

```bash
# For each file you plan to edit, replace <file> with the workspace-relative
# path. Repo is the qontinui repo name (e.g. qontinui-runner, qontinui-web).
curl -s "https://coord.qontinui.io/coord/builds/peers?repo=<repo>&since=30m&file=<file>" \
  | jq '.[] | select(.result == "failure" and .error_file == "<file>")'
```

Surface format if a peer is red on the same file:
> Peer `<hostname>` last built red on `<file>` at `<ended_at>` —
> `<error_summary>`. Consider whether your edit collides with their
> in-flight fix.

If the curl fails (coord down, no `~/.qontinui/machine.json` upstream, no
peers), this step is a graceful no-op — proceed normally. Coord lives
at `coord.qontinui.io` by default; override with `$COORD_URL`.

---

## Phase 3: Implement Fixes

For each category, implement fixes using these patterns:

### Fix: Dead Imports (Python)

Read the file and remove unused imports:

```python
# Before
from typing import List, Dict, Optional, Tuple, Any
from collections import defaultdict, Counter

# After (if only List and defaultdict are used)
from typing import List
from collections import defaultdict
```

### Fix: Dead Imports (TypeScript)

```typescript
// Before
import { useState, useEffect, useCallback, useMemo } from 'react';

// After (if only useState and useEffect are used)
import { useState, useEffect } from 'react';
```

### Fix: Circular Dependencies (Python)

Option 1: Use TYPE_CHECKING for type-only imports
```python
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .other_module import SomeClass

def process(item: "SomeClass") -> None:
    pass
```

Option 2: Move shared code to a common module
```python
# Before: module_a imports module_b, module_b imports module_a
# After: Both import from common module
from .common import SharedClass
```

### Fix: Circular Dependencies (Rust)

Move shared types to a common module:
```rust
// In common.rs
pub struct SharedType { ... }

// In module_a.rs
use crate::common::SharedType;

// In module_b.rs
use crate::common::SharedType;
```

### Fix: Security Issues (Python)

**Hardcoded Secrets:**
```python
# Before
API_KEY = "sk-1234567890abcdef"

# After
import os
API_KEY = os.environ.get("API_KEY")
```

**SQL Injection:**
```python
# Before
query = f"SELECT * FROM users WHERE id = {user_id}"

# After
query = "SELECT * FROM users WHERE id = ?"
cursor.execute(query, (user_id,))
```

### Fix: Missing Type Hints (Python)

```python
# Before
def process(data):
    return data.upper()

# After
def process(data: str) -> str:
    return data.upper()
```

### Fix: Missing Types (TypeScript)

```typescript
// Before
function process(data) {
    return data.toUpperCase();
}

// After
function process(data: string): string {
    return data.toUpperCase();
}
```

### Fix: God Classes (Python)

Split into focused classes:
```python
# Before: One class with 30 methods doing everything

# After: Multiple focused classes
class UserValidator:
    def validate_email(self, email: str) -> bool: ...
    def validate_password(self, password: str) -> bool: ...

class UserRepository:
    def create(self, user: User) -> User: ...
    def find_by_id(self, id: int) -> User: ...

class UserNotifier:
    def send_welcome_email(self, user: User) -> None: ...
    def send_password_reset(self, user: User) -> None: ...
```

### Fix: High Complexity Functions

Break into smaller functions:
```python
# Before: One function with complexity 25

# After: Multiple focused functions
def process_order(order: Order) -> Result:
    validated = validate_order(order)
    priced = calculate_pricing(validated)
    return finalize_order(priced)

def validate_order(order: Order) -> ValidatedOrder: ...
def calculate_pricing(order: ValidatedOrder) -> PricedOrder: ...
def finalize_order(order: PricedOrder) -> Result: ...
```

---

## Phase 4: Verify Fixes

After implementing fixes, verify they work:

```bash
# Python: Run linting and type checking
cd $PWD/qontinui-web/backend
poetry run black .
poetry run isort .
poetry run ruff check . --fix
poetry run mypy --package app

# TypeScript: Run linting
cd $PWD/qontinui-web/frontend
npm run lint:fix
npm run type-check

# Rust: Run checks
cd $PWD/qontinui-runner
cargo check
cargo clippy
```

---

## Phase 5: Re-run Analysis

Verify issues are resolved:

```bash
cd $PWD/qontinui-devtools

# Re-run the same analysis to confirm fixes
poetry run qontinui-devtools import check /path/to/code
poetry run qontinui-devtools quality dead-code /path/to/code
```

---

## Automated Fix Workflow

When running this command, follow this workflow:

1. **Analyze** - Run appropriate analysis tools
2. **Report** - Show summary of issues found
3. **Confirm** - Ask user which categories to fix
4. **Fix** - Implement fixes for confirmed categories
5. **Verify** - Run linting/type checking
6. **Re-analyze** - Confirm issues are resolved
7. **Commit** - Optionally commit the fixes

---

## Example: Fix qontinui-web Backend

```bash
# Step 1: Run analysis
cd $PWD/qontinui-devtools
poetry run qontinui-devtools analyze $PWD/qontinui-web/backend/app

# Step 2: Review output and identify:
# - 39 dead imports
# - 22 security issues
# - 76 god classes

# Step 3: Fix dead imports (safest, start here)
# Read each file with dead imports
# Remove the unused import statements

# Step 4: Fix security issues
# Move hardcoded secrets to environment variables
# Review each finding - some may be false positives

# Step 5: Verify
cd $PWD/qontinui-web/backend
poetry run black . && poetry run isort . && poetry run mypy --package app

# Step 6: Re-analyze to confirm
cd $PWD/qontinui-devtools
poetry run qontinui-devtools quality dead-code $PWD/qontinui-web/backend/app
```

---

## Safety Notes

- **Always review** before deleting "dead code" - it may be used via dynamic imports
- **Test after fixes** - Run the test suite to catch regressions
- **Commit incrementally** - Make small, focused commits for each fix category
- **Skip uncertain fixes** - If unsure, leave for manual review
- **Preserve exports** - Public APIs may look "dead" but are used externally
