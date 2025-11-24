#!/bin/bash
# Pre-commit hook to block commit messages with Claude attribution

commit_msg_file="$1"
commit_msg=$(cat "$commit_msg_file")

# Check for Claude attribution patterns
if echo "$commit_msg" | grep -q "Generated with \[Claude Code\]"; then
    echo "❌ ERROR: Commit message contains Claude attribution"
    echo "Please remove the '🤖 Generated with [Claude Code]' line from your commit message"
    exit 1
fi

if echo "$commit_msg" | grep -q "Co-Authored-By: Claude"; then
    echo "❌ ERROR: Commit message contains Claude co-author attribution"
    echo "Please remove the 'Co-Authored-By: Claude' line from your commit message"
    exit 1
fi

exit 0
