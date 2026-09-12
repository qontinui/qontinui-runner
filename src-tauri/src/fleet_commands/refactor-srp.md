# Refactor for Single Responsibility Principle

Identify and refactor code that violates the Single Responsibility Principle (SRP).

## Instructions

**IMPORTANT**: This command modifies code. Work incrementally, verify each change, and run tests frequently.

---

### Phase 1: Identify SRP Violations

#### Run Automated Analysis

```bash
cd $PWD/qontinui-devtools
poetry run qontinui-devtools architecture god-classes /path/to/project
```

#### Manual Identification

Look for these patterns:

1. **God Classes** - Classes doing too many things:
   - Many methods (>15)
   - Many instance variables (>10)
   - Low cohesion (methods don't use same attributes)
   - Multiple unrelated responsibilities

2. **God Functions** - Functions doing too much:
   - Many lines (>50)
   - Multiple levels of abstraction
   - Many parameters (>5)
   - Does parsing AND processing AND saving

3. **Mixed Concerns**:
   - Business logic mixed with I/O
   - Data access mixed with presentation
   - Validation mixed with processing

---

### Phase 2: Analyze Each Violation

For each identified violation:

1. **List all responsibilities** the code currently handles
2. **Group related responsibilities** into cohesive units
3. **Identify dependencies** between responsibilities
4. **Plan the extraction** - which classes/functions to create

Example analysis:
```markdown
## UserService (God Class)

Current Responsibilities:
1. User CRUD operations
2. Password hashing
3. Email sending
4. Session management
5. Permission checking
6. Activity logging

Proposed Split:
- UserRepository: CRUD operations
- PasswordService: Hashing, validation
- EmailService: Sending emails (probably exists)
- SessionManager: Session handling
- PermissionChecker: Authorization
- ActivityLogger: Logging (probably exists)
```

---

### Phase 3: Refactoring Patterns

#### Extract Class
```python
# Before: God class
class OrderProcessor:
    def process_order(self, order):
        # Validate order
        if not order.items:
            raise ValueError("Empty order")

        # Calculate totals
        subtotal = sum(item.price for item in order.items)
        tax = subtotal * 0.1
        total = subtotal + tax

        # Save to database
        db.save(order)

        # Send email
        send_email(order.customer, "Order confirmed")

# After: Separated responsibilities
class OrderValidator:
    def validate(self, order: Order) -> None:
        if not order.items:
            raise ValueError("Empty order")

class OrderCalculator:
    def calculate_totals(self, order: Order) -> OrderTotals:
        subtotal = sum(item.price for item in order.items)
        tax = subtotal * 0.1
        return OrderTotals(subtotal=subtotal, tax=tax, total=subtotal + tax)

class OrderRepository:
    def save(self, order: Order) -> None:
        db.save(order)

class OrderNotifier:
    def notify_confirmation(self, order: Order) -> None:
        send_email(order.customer, "Order confirmed")

class OrderProcessor:
    def __init__(
        self,
        validator: OrderValidator,
        calculator: OrderCalculator,
        repository: OrderRepository,
        notifier: OrderNotifier,
    ):
        self.validator = validator
        self.calculator = calculator
        self.repository = repository
        self.notifier = notifier

    def process_order(self, order: Order) -> None:
        self.validator.validate(order)
        order.totals = self.calculator.calculate_totals(order)
        self.repository.save(order)
        self.notifier.notify_confirmation(order)
```

#### Extract Function
```python
# Before: Function doing too much
def process_data(filepath):
    # Read file
    with open(filepath) as f:
        raw = f.read()

    # Parse data
    lines = raw.split('\n')
    records = [line.split(',') for line in lines]

    # Transform
    result = []
    for record in records:
        result.append({
            'name': record[0].strip(),
            'value': int(record[1])
        })

    # Save
    with open('output.json', 'w') as f:
        json.dump(result, f)

# After: Separated functions
def read_file(filepath: str) -> str:
    with open(filepath) as f:
        return f.read()

def parse_csv(raw: str) -> list[list[str]]:
    lines = raw.split('\n')
    return [line.split(',') for line in lines]

def transform_records(records: list[list[str]]) -> list[dict]:
    return [
        {'name': record[0].strip(), 'value': int(record[1])}
        for record in records
    ]

def save_json(data: list[dict], filepath: str) -> None:
    with open(filepath, 'w') as f:
        json.dump(data, f)

def process_data(filepath: str) -> None:
    raw = read_file(filepath)
    records = parse_csv(raw)
    transformed = transform_records(records)
    save_json(transformed, 'output.json')
```

---

### Phase 4: Refactoring Process

For each violation:

1. **Create new file(s)** for extracted responsibilities
2. **Move code** to new locations
3. **Update imports** in all affected files
4. **Run tests** to verify behavior unchanged
5. **Run linters** to catch any issues

```bash
# After each refactor
poetry run pytest
poetry run mypy --package <package>
poetry run ruff check .
```

---

### Phase 5: Update Dependencies

When extracting classes:

1. **Use dependency injection** for flexibility
2. **Create interfaces/protocols** if needed
3. **Update constructors** to accept dependencies
4. **Update all callers** to provide dependencies

---

### Phase 6: Verify Refactoring

After completing refactoring:

1. **All tests pass**: `poetry run pytest`
2. **No type errors**: `poetry run mypy --package <package>`
3. **No lint errors**: `poetry run ruff check .`
4. **Re-run god class check**:
   ```bash
   poetry run qontinui-devtools architecture god-classes /path/to/project
   ```

---

### Phase 7: Parallel Processing

For large codebases:

1. **Prioritize violations** by severity (LCOM score, method count)
2. **Group independent refactors** that don't overlap
3. **Use Task agents** to refactor in parallel
4. **Merge and test** after each batch

---

### Notes

- Preserve git history with small, focused commits
- Don't change functionality while refactoring
- If tests are missing, add them BEFORE refactoring
- Document the new structure in module docstrings
- Update any affected documentation
