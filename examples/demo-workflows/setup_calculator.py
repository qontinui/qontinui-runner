"""Setup script for the 'Fix Buggy Calculator' demo workflow.

Creates a Python calculator module with deliberate bugs and a test file
that verifies correct behavior. The AI must find and fix the bugs.
"""
import pathlib

WORKSPACE = pathlib.Path(r"C:/temp/qontinui-demo/calculator")
WORKSPACE.mkdir(parents=True, exist_ok=True)

# Calculator with deliberate bugs
(WORKSPACE / "calculator.py").write_text('''\
def add(a, b):
    """Add two numbers."""
    return a + b


def subtract(a, b):
    """Subtract b from a."""
    return a + b  # BUG: should be a - b


def multiply(a, b):
    """Multiply two numbers."""
    return a * b


def divide(a, b):
    """Divide a by b. Returns None if b is zero."""
    if b == 0:
        return None
    return a * b  # BUG: should be a / b


def power(a, b):
    """Raise a to the power of b."""
    return a ** b


def modulo(a, b):
    """Return a modulo b. Returns None if b is zero."""
    if b == 0:
        return None
    return a // b  # BUG: should be a % b
''', encoding='utf-8')

# Test file with correct expectations
(WORKSPACE / "test_calculator.py").write_text('''\
"""Tests for the calculator module."""
import sys
from calculator import add, subtract, multiply, divide, power, modulo

failures = []


def assert_equal(actual, expected, test_name):
    if actual != expected:
        failures.append(f"FAIL: {test_name} - expected {expected}, got {actual}")
    else:
        print(f"PASS: {test_name}")


# Test add
assert_equal(add(2, 3), 5, "add(2, 3)")
assert_equal(add(-1, 1), 0, "add(-1, 1)")
assert_equal(add(0, 0), 0, "add(0, 0)")

# Test subtract
assert_equal(subtract(10, 3), 7, "subtract(10, 3)")
assert_equal(subtract(0, 5), -5, "subtract(0, 5)")
assert_equal(subtract(-3, -3), 0, "subtract(-3, -3)")

# Test multiply
assert_equal(multiply(4, 5), 20, "multiply(4, 5)")
assert_equal(multiply(-2, 3), -6, "multiply(-2, 3)")
assert_equal(multiply(0, 100), 0, "multiply(0, 100)")

# Test divide
assert_equal(divide(10, 2), 5.0, "divide(10, 2)")
assert_equal(divide(7, 2), 3.5, "divide(7, 2)")
assert_equal(divide(0, 5), 0.0, "divide(0, 5)")
assert_equal(divide(10, 0), None, "divide(10, 0) - division by zero")

# Test power
assert_equal(power(2, 3), 8, "power(2, 3)")
assert_equal(power(5, 0), 1, "power(5, 0)")

# Test modulo
assert_equal(modulo(10, 3), 1, "modulo(10, 3)")
assert_equal(modulo(15, 5), 0, "modulo(15, 5)")
assert_equal(modulo(7, 0), None, "modulo(7, 0) - modulo by zero")

# Summary
if failures:
    print(f"\\n{len(failures)} test(s) FAILED:")
    for f in failures:
        print(f"  {f}")
    sys.exit(1)
else:
    print(f"\\nAll tests passed!")
    sys.exit(0)
''', encoding='utf-8')

print(f"Demo workspace created at: {WORKSPACE}")
print("Files: calculator.py (with 3 bugs), test_calculator.py")
