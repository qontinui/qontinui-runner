# Review Before Code

Pre-implementation architectural review for complex features.

## When to Use This Command

**Use this for:**
- New features spanning multiple files/modules
- Refactoring existing architecture
- Performance-sensitive implementations
- Complex integrations between Qontinui components
- Features that will be hard to debug if implemented poorly

**Skip for:**
- Simple bug fixes (use `/debug` instead)
- One-line changes
- Trivial additions
- Documentation updates

## Review Process

### 1. Understand Requirements

Ask clarifying questions:
- What is the core functionality needed?
- What are the edge cases?
- What are the performance requirements?
- What are the success criteria?

Read relevant documentation:
- knowledge-base/qontinui-specific/architecture.md
- Project-specific CLAUDE.md
- Related code in the codebase

### 2. Architecture Assessment

Evaluate architectural fit:
- Does this fit existing architecture patterns?
- Should this be a new module or extend existing code?
- What dependencies are required?
- Impact on other Qontinui repos?
- Does this violate any architectural principles?

Check knowledge base:
- knowledge-base/best-practices/[language].md for language-specific patterns
- knowledge-base/qontinui-specific/architecture.md for ecosystem constraints

### 3. Debuggability Analysis

**This is critical for the debugging strategy.**

Ask yourself:
- **Testability**: Can this be tested in isolation?
- **Modularity**: Is each component focused and single-purpose?
- **Observability**: Where should logging be added?
- **Fail-fast**: What assertions would catch bugs early?
- **SRP**: Does each function/class have a single responsibility?

Red flags that indicate poor debuggability:
- Functions longer than 50 lines
- Deeply nested conditionals (>3 levels)
- Mixed concerns (business logic + I/O + validation in one place)
- Hidden state mutations
- Complex conditional logic without named helpers
- No clear error messages

### 4. Logging Strategy

**Plan logging BEFORE coding:**

Identify where to add:
- Entry/exit logging for key functions
- State change logging
- Decision point logging (why did we take this branch?)
- Error context logging (what state were we in when error occurred?)

Plan log levels:
- DEBUG: Detailed flow for development
- INFO: Key milestones and state changes
- WARNING: Unexpected but handled situations
- ERROR: Failures that need attention

Example logging plan:
```python
# Planned logs for feature: workflow execution engine

def execute_workflow(workflow_id):
    logger.info(f"Executing workflow {workflow_id}")  # Entry

    workflow = load_workflow(workflow_id)
    logger.debug(f"Loaded workflow: {workflow.name}, steps={len(workflow.steps)}")

    for step in workflow.steps:
        logger.debug(f"Executing step: {step.name}")
        # ... execution logic
        logger.info(f"Step {step.name} completed: {result}")

    logger.info(f"Workflow {workflow_id} execution complete")  # Success
```

### 5. Break Down Implementation

Create implementation plan with:

**Testable units** (functions/classes):
- List each function/class to create
- Define inputs, outputs, and side effects
- Plan unit tests for each

**Integration points**:
- How components connect
- API contracts between modules
- Data flow between components

**Validation steps**:
- After each unit: run unit tests
- After integration: run integration tests
- After completion: run full test suite

**Test scenarios**:
- Happy path
- Edge cases
- Error cases
- Performance tests (if applicable)

### 6. Refactoring Decision

**Per Qontinui philosophy: "Refactor aggressively to improve code quality"**

If existing code is:
- Hard to extend (tight coupling)
- Violates SRP (doing too much)
- Poorly testable (hidden dependencies)
- Missing error handling
- Confusing or unclear

**Decision: Refactor FIRST, then add feature.**

Don't work around bad code. Fix it.

### 7. Implementation Plan Template

Provide output in this format:

```markdown
## Feature: [Name]

### Requirements Summary
- [Key requirement 1]
- [Key requirement 2]
- [Edge cases identified]

### Architecture Decision
[New module / Extend existing / Refactor then extend]

Rationale: [Why this approach]

### Components to Create/Modify

#### 1. [Component Name]
- **Purpose**: [Single responsibility]
- **Location**: [File path]
- **Key functions**:
  - `function_name(params) -> return`: [What it does]
- **Logging**: [Entry/exit/state changes]
- **Tests**: [Test scenarios]

#### 2. [Component Name]
...

### Integration Points
- [Component A] → [Component B]: [How they connect]
- [API contract]: [Data format]

### Logging Strategy
- Entry points: [Functions to log]
- State changes: [What to track]
- Errors: [What context to include]

### Test Plan
1. Unit tests for [components]
2. Integration test for [flow]
3. Edge case tests for [scenarios]

### Potential Pitfalls
- [Known issue from knowledge base]
- [Architectural concern]
- [Performance consideration]

### Implementation Order
1. [Refactor X if needed]
2. [Create component A]
3. [Test component A]
4. [Create component B]
5. [Test integration]
6. [Add logging]
7. [Full test suite]

### Success Criteria
- [ ] All tests pass
- [ ] Logging in place for debugging
- [ ] No SRP violations
- [ ] Edge cases handled
- [ ] Performance acceptable
```

### 8. Review Against Best Practices

Check against:
- knowledge-base/best-practices/python.md (if Python)
- knowledge-base/best-practices/typescript.md (if TypeScript)
- knowledge-base/best-practices/rust.md (if Rust)
- knowledge-base/qontinui-specific/common-pitfalls.md

### 9. Get User Approval

Present the plan and ask:
- Does this match your vision?
- Any concerns about the approach?
- Should we adjust anything before coding?

Wait for approval before proceeding to implementation.

## After Review: Next Steps

Once plan is approved:
1. Use the plan to guide implementation
2. Follow the test-first approach outlined
3. Add logging as planned
4. Validate after each step
5. Use `/debug` if issues arise during implementation

## Benefits of This Approach

✓ Catches architectural issues before coding
✓ Plans for debuggability from the start
✓ Identifies refactoring needs early
✓ Creates clear testing strategy
✓ Produces implementation roadmap
✓ Reduces debugging time later
✓ Aligns with Qontinui's "refactor aggressively" philosophy
