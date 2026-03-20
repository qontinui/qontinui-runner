## UI Bridge Control - Custom App Template

Use this template to document UI Bridge elements for your own application. Copy this to a User Context and customize it.

### Connection

```python
from ui_bridge import UIBridgeClient

# Your app's UI Bridge server
client = UIBridgeClient("http://localhost:YOUR_PORT")
```

### Available Elements

Document your app's elements here:

| Element ID | Type | Description |
|------------|------|-------------|
| `your-element-id` | button | Description |

### Available Components

Document your app's components here:

**Component:** `your-component-id`

| Action ID | Description | Parameters |
|-----------|-------------|------------|
| `action-name` | What it does | `param1`, `param2` |

### Common Automation Patterns

Add patterns specific to your app:

```python
# Example pattern
client.click("your-element")
```

### Discovery Script

Run this to discover your app's elements:

```python
from ui_bridge import UIBridgeClient

client = UIBridgeClient("http://localhost:YOUR_PORT")

print("=== Elements ===")
for elem in client.get_elements():
    print(f"| `{elem.id}` | {elem.type} | {elem.label} |")

print("\n=== Components ===")
for comp in client.get_components():
    print(f"\n**Component:** `{comp.id}` - {comp.name}")
    for action in comp.actions:
        print(f"- `{action.id}`: {action.label}")
```

### Tips for Documenting Your App

1. **Run discovery first** - Use the script above to find all elements
2. **Group by feature** - Organize elements by screens/features
3. **Document state** - Note what state properties are available
4. **Add patterns** - Include common automation sequences
5. **Keep updated** - Update docs when you add new elements
