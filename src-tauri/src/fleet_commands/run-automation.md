# Run Qontinui Automation

Execute a Qontinui automation workflow using the qontinui-http.py helper script.

## Usage

```
/run-automation <config_path> [workflow_name]
```

Arguments:
- `config_path` - Absolute path to the JSON configuration file
- `workflow_name` - (Optional) Name of specific workflow to run. If omitted, runs the first workflow.

## Instructions

Use the qontinui-http.py script to communicate directly with the qontinui-runner via HTTP.

**Script location:** `$PWD/qontinui-claude-config/scripts/qontinui-http.py`

### Step 1: Check Runner Status

First, verify the runner is running:
```bash
python $PWD/qontinui-claude-config/scripts/qontinui-http.py status
```

If the runner is not available, inform the user:
```
The qontinui-runner is not running. Start it with:
  .\dev-start.ps1 -Runner
```

### Step 2: Load the Configuration

Load the workflow configuration file:
```bash
python $PWD/qontinui-claude-config/scripts/qontinui-http.py load-config "<config_path>"
```

The script automatically converts WSL paths to Windows paths.

### Step 3: Run the Workflow

Run the specified workflow (or first workflow if none specified):
```bash
python $PWD/qontinui-claude-config/scripts/qontinui-http.py run-workflow "<workflow_name>"
```

Optional: specify a monitor:
```bash
python $PWD/qontinui-claude-config/scripts/qontinui-http.py run-workflow "<workflow_name>" --monitor left
```

Monitor options: `left`, `right`, `primary`, or an index (0, 1, 2).

### Step 4: Report Results

The `run-workflow` command returns JSON with the execution result:
```json
{
  "success": true,
  "data": {
    "success": true,
    "workflow_name": "MyWorkflow",
    "error": null
  }
}
```

Parse the JSON output and report:
- `data.success` - Whether the workflow completed successfully
- `data.workflow_name` - Name of the workflow that ran
- `data.error` - Error message if failed (null on success)

## Results Location

Automation results are saved to `.automation-results/` in versioned folders:
- Screenshots captured during execution
- Rich details of each action
- Execution logs and timing information

Check the latest results folder for debugging failed automations.

### Step 5: QA Follow-up (if failed)

If the automation failed, inform the user they can run `/qa` to analyze failures and fix issues:
```
Automation failed. Run /qa to analyze the failures and fix issues.
```

## Available Commands

The qontinui-http.py script supports these commands:

| Command | Description |
|---------|-------------|
| `status` | Get runner status |
| `health` | Health check |
| `monitors` | List available monitors |
| `load-config <path>` | Load a workflow configuration file |
| `run-workflow <name>` | Run a workflow by name |
| `stop` | Stop current execution |

## Example

```
/run-automation $PWD/qontinui-runner/configs/test-workflow.json
```

Or with a specific workflow:
```
/run-automation $PWD/qontinui-runner/configs/multi-workflow.json LoginWorkflow
```

## Arguments

$ARGUMENTS
