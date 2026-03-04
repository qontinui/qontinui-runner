# Qontinui Runner - Presentation Demo Script

## Prerequisites

Before the demo:
1. Runner is running (`npm run tauri dev` or release build)
2. Claude CLI is installed and authenticated (`claude --version` works)
3. Python is installed (`python --version` works)
4. Demo workflows exist (either seeded on first launch, or run `create_demo_workflows.ps1`)
5. Reset demo workspaces by running the calculator workflow once (setup step resets files)

## Demo Overview (~10-15 minutes)

| Section | Duration | What to Show |
|---------|----------|-------------|
| 1. Introduction | 1 min | What is Qontinui? |
| 2. Run a Demo Workflow | 3-4 min | Fix Buggy Calculator — watch the loop |
| 3. Show the Results | 1 min | Verification passed, code was fixed |
| 4. Generate a Workflow | 3-4 min | AI generates from natural language |
| 5. TDD Demo (optional) | 2-3 min | Implement from Tests — more complex |
| 6. Architecture Overview | 2 min | How it works under the hood |

---

## Section 1: Introduction (1 minute)

**Navigate to:** Execute page (workflow-queue)

**Talking points:**
- "Qontinui is a visual automation platform that uses AI to automate software development tasks"
- "The core concept: you define what needs to be TRUE (verification checks), and the AI figures out HOW to make it true"
- "Think of it as CI/CD meets AI — the runner continuously tests and fixes your code"

---

## Section 2: Run "Fix Buggy Calculator" (3-4 minutes)

### Step 2a: Show the workflow
**Navigate to:** Execute page → Find "Demo: Fix Buggy Calculator" in the workflow library

**Talking points:**
- "Here's a simple demo. We have a Python calculator with 3 bugs"
- "The workflow has 4 phases:"
  - "**Setup**: Creates the broken files (resets workspace)"
  - "**Verification**: Runs the test suite — expects failures"
  - "**Agentic**: AI reads the failures and fixes the code"
  - "**Verification re-run**: Tests run again — should pass"

### Step 2b: Run the workflow
**Action:** Click "Run" on the calculator workflow (or add to queue and start)

**Navigate to:** Active dashboard (automatically or manually)

**Talking points while watching:**
- "Watch the phases in real-time..."
- "Setup phase just ran — it created the buggy calculator and test files"
- "Now verification runs... 7 out of 18 tests fail"
- "The agentic phase kicks in — Claude CLI reads the test failures and edits calculator.py"
- "Verification runs again... all 18 tests pass!"
- "Total time: about 30 seconds for the entire loop"

### Step 2c: Show what the AI did
**After completion, optionally show:**
- The test output (all PASS)
- The fixed calculator.py (if visible in the results)

**Talking point:**
- "The AI identified 3 bugs: subtract was adding instead of subtracting, divide was multiplying, and modulo was doing integer division"
- "It's not just running tests — it's understanding the intent from the tests and fixing the root cause"

---

## Section 3: Show the Results (1 minute)

**Navigate to:** Runs → Summary tab

**Talking points:**
- "Every run is tracked with full detail"
- "You can see the AI's output, which files it modified, how many iterations it took"
- "The verification results show exactly which checks passed/failed at each iteration"

---

## Section 4: Generate a Workflow from Natural Language (3-4 minutes)

### Step 4a: Open the generator
**Navigate to:** Terminal page → Generate Workflow button, or Workflow Builder → AI Generate panel

### Step 4b: Type a description
**Type something like:**
> "Create a workflow that checks if a Python function correctly implements FizzBuzz.
> The function should return 'Fizz' for multiples of 3, 'Buzz' for multiples of 5,
> 'FizzBuzz' for multiples of both, and the number as a string otherwise.
> Create the test file and an empty implementation, then let the AI implement it."

### Step 4c: Click "Generate"
**Talking points while waiting (~30-45 seconds):**
- "The generator uses a multi-agent pipeline"
- "First, a builder agent creates the workflow JSON from my description"
- "Then a verifier agent checks the workflow structure"
- "If there are issues, a fixer agent corrects them"
- "This is the same verification-agentic loop, applied to workflow creation itself"

### Step 4d: Show the generated workflow
**Navigate to:** The generated workflow in the builder

**Talking points:**
- "The AI created a complete workflow with setup, verification, and agentic steps"
- "Setup creates the test file and empty implementation"
- "Verification runs the tests"
- "The agentic prompt tells the AI to implement FizzBuzz"
- "You can edit any step, add more checks, change the prompt"

### Step 4e: (Optional) Run the generated workflow
**Action:** Click "Run" to see it work

---

## Section 5: TDD Demo — "Implement from Tests" (Optional, 2-3 minutes)

### If time permits, run the TDD workflow:

**Navigate to:** Execute → "Demo: Implement from Tests (TDD)"

**Talking points:**
- "This is a more complex demo — 5 functions need to be implemented from scratch"
- "The test file is the specification — it defines exactly what each function should do"
- "The AI reads the tests, understands the requirements, and writes all 5 functions"
- "reverse_words, title_case, count_vowels, is_palindrome, truncate"

**Run and watch:**
- Verification fails (ImportError — functions don't exist)
- AI implements all 5 functions in string_utils.py
- Verification passes (22 tests)

**Talking point:**
- "One iteration, 30 seconds, 5 fully-implemented functions — all from test specifications"

---

## Section 6: Architecture Overview (2 minutes)

**Show a slide or describe verbally:**

```
┌─────────────────────────────────────────┐
│           Qontinui Runner               │
│                                         │
│  ┌─────────┐  ┌──────────┐  ┌────────┐ │
│  │  Setup   │→│Verification│→│Agentic │ │
│  │  Phase   │  │  Phase    │  │ Phase  │ │
│  └─────────┘  └────┬─────┘  └───┬────┘ │
│                     │    ↑       │      │
│                     │    └───────┘      │
│                     │  (loop until      │
│                     │   checks pass)    │
│                     ↓                   │
│               ┌──────────┐              │
│               │Completion│              │
│               │  Phase   │              │
│               └──────────┘              │
└─────────────────────────────────────────┘
         │                    │
         ↓                    ↓
   ┌──────────┐        ┌──────────┐
   │  Claude   │        │  Shell   │
   │   CLI     │        │ Commands │
   │ (AI agent)│        │ (checks) │
   └──────────┘        └──────────┘
```

**Talking points:**
- "The runner is a Tauri desktop app — Rust backend + React frontend"
- "Verification steps are shell commands: tests, linters, type checkers — anything with an exit code"
- "The agentic phase spawns Claude CLI with full file system access"
- "Claude reads the test failures, understands the code, and makes targeted fixes"
- "The loop continues until all checks pass or max iterations is reached"
- "No Python dependency — the entire execution engine is in Rust"

---

## Backup Demo: Fix Data Pipeline

If one of the above demos fails, use this as a backup:

**Workflow:** "Demo: Fix Data Pipeline"
- Sales data CSV → Python pipeline with revenue calculation bugs → JSON report
- Validation checks against manually calculated expected values
- AI fixes: type conversion (string→float), arithmetic (multiply→divide), min→max

---

## Quick Reference: Demo Workflow IDs

| Workflow | Description | Time | Complexity |
|----------|------------|------|-----------|
| Fix Buggy Calculator | 3 arithmetic bugs | ~30s | Simple |
| Implement from Tests (TDD) | 5 functions from scratch | ~35s | Medium |
| Fix Data Pipeline | Revenue + aggregation bugs | ~40s | Complex |

## Troubleshooting

| Issue | Fix |
|-------|-----|
| "Claude CLI not found" | Run `claude --version` to verify, check Settings → AI |
| Workflow fails in setup | Check Python is in PATH: `python --version` |
| AI takes too long | Check Claude CLI auth: `claude "hello"` |
| Demo workflows missing | Run `create_demo_workflows.ps1` or restart runner (auto-seeds) |
| Runner not starting | Check port 9876: `curl http://localhost:9876/health` |
