#!/usr/bin/env python3
"""
Verification script for RunnerOrchestrator implementation.

This script verifies that:
1. RunnerOrchestrator class is properly defined
2. All required methods exist
3. Command binding functions are available
4. Type hints are correct
"""

import sys
import inspect
from pathlib import Path

# Add current directory to path
sys.path.insert(0, str(Path(__file__).parent))


def verify_class_exists():
    """Verify RunnerOrchestrator class exists."""
    try:
        from qontinui_executor import RunnerOrchestrator

        print("✓ RunnerOrchestrator class exists")
        return True, RunnerOrchestrator
    except ImportError as e:
        print(f"✗ Failed to import RunnerOrchestrator: {e}")
        return False, None


def verify_methods(cls):
    """Verify all required methods exist."""
    required_methods = [
        "__init__",
        "_load_config",
        "_initialize_hal",
        "execute_transition",
        "navigate_to_state",
        "navigate_to_multiple_states",
        "get_active_states",
        "get_available_transitions",
        "_emit_to_tauri",
    ]

    missing = []
    for method in required_methods:
        if not hasattr(cls, method):
            missing.append(method)

    if missing:
        print(f"✗ Missing methods: {', '.join(missing)}")
        return False
    else:
        print(f"✓ All {len(required_methods)} required methods exist")
        return True


def verify_type_hints(cls):
    """Verify methods have type hints."""
    methods_to_check = [
        "execute_transition",
        "navigate_to_state",
        "navigate_to_multiple_states",
        "get_active_states",
        "get_available_transitions",
    ]

    issues = []
    for method_name in methods_to_check:
        method = getattr(cls, method_name)
        sig = inspect.signature(method)

        # Check return annotation
        if sig.return_annotation == inspect.Signature.empty:
            issues.append(f"{method_name} missing return type hint")

        # Check parameter annotations
        for param_name, param in sig.parameters.items():
            if param_name == "self":
                continue
            if param.annotation == inspect.Parameter.empty:
                issues.append(f"{method_name} parameter '{param_name}' missing type hint")

    if issues:
        print(f"✗ Type hint issues:")
        for issue in issues:
            print(f"  - {issue}")
        return False
    else:
        print(f"✓ All methods have proper type hints")
        return True


def verify_command_bindings():
    """Verify command binding functions exist."""
    try:
        from qontinui_executor import (
            initialize_orchestrator,
            get_orchestrator,
            execute_transition,
            navigate_to_state,
            navigate_to_multiple_states,
            get_active_states,
            get_available_transitions,
        )

        print("✓ All command binding functions exist")
        return True
    except ImportError as e:
        print(f"✗ Failed to import command bindings: {e}")
        return False


def verify_docstrings(cls):
    """Verify all public methods have docstrings."""
    public_methods = [name for name in dir(cls) if not name.startswith("_") or name == "__init__"]
    missing_docs = []

    for method_name in public_methods:
        if callable(getattr(cls, method_name)):
            method = getattr(cls, method_name)
            if not method.__doc__:
                missing_docs.append(method_name)

    if missing_docs:
        print(f"⚠ Methods missing docstrings: {', '.join(missing_docs)}")
        return False
    else:
        print("✓ All public methods have docstrings")
        return True


def main():
    """Run all verification checks."""
    print("=" * 60)
    print("RunnerOrchestrator Verification")
    print("=" * 60)
    print()

    all_passed = True

    # Check 1: Class exists
    print("Check 1: Class Definition")
    exists, cls = verify_class_exists()
    all_passed = all_passed and exists
    print()

    if not exists:
        print("Cannot continue without class definition")
        sys.exit(1)

    # Check 2: Required methods
    print("Check 2: Required Methods")
    all_passed = all_passed and verify_methods(cls)
    print()

    # Check 3: Type hints
    print("Check 3: Type Hints")
    all_passed = all_passed and verify_type_hints(cls)
    print()

    # Check 4: Command bindings
    print("Check 4: Command Bindings")
    all_passed = all_passed and verify_command_bindings()
    print()

    # Check 5: Docstrings
    print("Check 5: Documentation")
    all_passed = all_passed and verify_docstrings(cls)
    print()

    # Summary
    print("=" * 60)
    if all_passed:
        print("✓ ALL CHECKS PASSED")
        print("RunnerOrchestrator is properly implemented!")
        return 0
    else:
        print("✗ SOME CHECKS FAILED")
        print("Please fix the issues above.")
        return 1


if __name__ == "__main__":
    sys.exit(main())
