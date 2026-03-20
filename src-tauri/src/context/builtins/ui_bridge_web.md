## UI Bridge Control - qontinui-web

This context documents the UI Bridge elements available in the qontinui-web Next.js application.

### Connection

```python
from ui_bridge import UIBridgeClient

# Development server (check your frontend port)
client = UIBridgeClient("http://localhost:3001")

# Production (if UI Bridge server is enabled)
client = UIBridgeClient("https://your-domain.com")
```

### Available Elements

#### Navigation

| Element ID      | Type   | Description             |
| --------------- | ------ | ----------------------- |
| `nav-projects`  | button | Navigate to projects    |
| `nav-workflows` | button | Navigate to workflows   |
| `nav-runs`      | button | Navigate to run history |
| `nav-settings`  | button | Navigate to settings    |

#### Project Management

| Element ID                 | Type      | Description          |
| -------------------------- | --------- | -------------------- |
| `project-card-{projectId}` | pressable | Project card in list |
| `project-create-btn`       | button    | Create new project   |
| `project-edit-btn`         | button    | Edit current project |
| `project-delete-btn`       | button    | Delete project       |

#### Workflow Builder

| Element ID            | Type   | Description          |
| --------------------- | ------ | -------------------- |
| `workflow-canvas`     | view   | Main workflow canvas |
| `workflow-save-btn`   | button | Save workflow        |
| `workflow-run-btn`    | button | Run workflow         |
| `workflow-export-btn` | button | Export workflow      |

#### State Explorer

| Element ID                       | Type      | Description               |
| -------------------------------- | --------- | ------------------------- |
| `state-node-{stateId}`           | pressable | State node in explorer    |
| `transition-edge-{transitionId}` | view      | Transition between states |
| `explorer-zoom-in`               | button    | Zoom in                   |
| `explorer-zoom-out`              | button    | Zoom out                  |
| `explorer-fit-view`              | button    | Fit to view               |

#### Forms

| Element ID               | Type   | Description      |
| ------------------------ | ------ | ---------------- |
| `form-{formName}-submit` | button | Submit form      |
| `form-{formName}-cancel` | button | Cancel form      |
| `input-{fieldName}`      | input  | Form input field |

### Common Automation Patterns

**Create a new project:**

```python
client.click("project-create-btn")
client.type("input-project-name", "My New Project")
client.type("input-project-description", "Project description")
client.click("form-create-project-submit")
```

**Navigate to a specific project:**

```python
elements = client.get_elements()
for elem in elements:
    if elem.id.startswith("project-card-") and "MyProject" in elem.state.text:
        client.click(elem.id)
        break
```

**Run a workflow:**

```python
client.click("workflow-run-btn")
# Wait for run to start
time.sleep(2)
```

**Export workflow configuration:**

```python
client.click("workflow-export-btn")
# Handle file download dialog if needed
```

### App Structure

```
qontinui-web/frontend/
├── src/
│   ├── app/
│   │   ├── layout.tsx           # UIBridgeProvider here
│   │   ├── projects/
│   │   │   └── page.tsx         # project-* elements
│   │   ├── workflows/
│   │   │   └── [id]/page.tsx    # workflow-* elements
│   │   └── explorer/
│   │       └── page.tsx         # state-*, transition-* elements
│   └── components/
│       ├── projects/ProjectCard.tsx
│       ├── workflows/WorkflowCanvas.tsx
│       └── explorer/StateExplorer.tsx
```

### Notes

- UI Bridge is typically only enabled in development mode
- Some elements may only appear after certain user actions (e.g., opening modals)
- Check the actual element IDs by calling `client.get_elements()` if elements have changed
