"""Setup script for the 'Implement from Tests (TDD)' demo workflow.

Creates a test file with tests for string utility functions that don't
exist yet. The AI must implement the functions to make all tests pass.
"""
import pathlib

WORKSPACE = pathlib.Path(r"C:/temp/qontinui-demo/string-utils")
WORKSPACE.mkdir(parents=True, exist_ok=True)

# Empty module - AI must implement these functions
(WORKSPACE / "string_utils.py").write_text('''\
"""String utility functions.

TODO: Implement the functions below. See test_string_utils.py for requirements.
"""
''', encoding='utf-8')

# Comprehensive test file
(WORKSPACE / "test_string_utils.py").write_text('''\
"""Tests for string utility functions."""
import sys
from string_utils import (
    reverse_words,
    title_case,
    count_vowels,
    is_palindrome,
    truncate,
)

failures = []


def assert_equal(actual, expected, test_name):
    if actual != expected:
        failures.append(f"FAIL: {test_name} - expected {expected!r}, got {actual!r}")
    else:
        print(f"PASS: {test_name}")


# Test reverse_words: reverse the order of words in a string
assert_equal(reverse_words("hello world"), "world hello", "reverse_words basic")
assert_equal(reverse_words("one"), "one", "reverse_words single word")
assert_equal(reverse_words("a b c d"), "d c b a", "reverse_words multiple")
assert_equal(reverse_words(""), "", "reverse_words empty")

# Test title_case: capitalize first letter of each word, lowercase rest
assert_equal(title_case("hello world"), "Hello World", "title_case basic")
assert_equal(title_case("HELLO WORLD"), "Hello World", "title_case from upper")
assert_equal(title_case("python is fun"), "Python Is Fun", "title_case sentence")
assert_equal(title_case(""), "", "title_case empty")

# Test count_vowels: count vowels (a, e, i, o, u) case-insensitive
assert_equal(count_vowels("hello"), 2, "count_vowels hello")
assert_equal(count_vowels("AEIOU"), 5, "count_vowels all upper")
assert_equal(count_vowels("bcdfg"), 0, "count_vowels none")
assert_equal(count_vowels(""), 0, "count_vowels empty")

# Test is_palindrome: check if string is a palindrome (ignore case and spaces)
assert_equal(is_palindrome("racecar"), True, "is_palindrome racecar")
assert_equal(is_palindrome("Race Car"), True, "is_palindrome ignore case/spaces")
assert_equal(is_palindrome("hello"), False, "is_palindrome hello")
assert_equal(is_palindrome("A man a plan a canal Panama"),
             True, "is_palindrome famous phrase")
assert_equal(is_palindrome(""), True, "is_palindrome empty")

# Test truncate: truncate string to max_length, add "..." if truncated
assert_equal(truncate("hello world", 5), "he...", "truncate short")
assert_equal(truncate("hello", 10), "hello", "truncate no-op")
assert_equal(truncate("hello", 5), "hello", "truncate exact")
assert_equal(truncate("hello world", 8), "hello...", "truncate mid")
assert_equal(truncate("", 5), "", "truncate empty")

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
print("Files: string_utils.py (empty), test_string_utils.py (with tests)")
