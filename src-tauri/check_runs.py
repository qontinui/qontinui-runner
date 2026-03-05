import urllib.request
import json

# Get task runs
resp = urllib.request.urlopen('http://localhost:9876/task-runs')
data = json.loads(resp.read().decode())
runs = data.get('data', data) if isinstance(data, dict) else data

# Find reflection runs
reflections = [r for r in runs if 'Reflection' in str(r.get('workflow_name', ''))]
print(f'Total runs: {len(runs)}, Reflection runs: {len(reflections)}')

for r in reflections[:10]:
    rid = str(r.get('id', ''))[:8]
    name = str(r.get('workflow_name', ''))[:70]
    date = str(r.get('created_at', ''))[:19]
    status = r.get('status', '')
    print(f'  {rid} | {status} | {date} | {name}')

print()

# Show recent runs
print('Recent 6 runs:')
for r in runs[:6]:
    rid = str(r.get('id', ''))[:8]
    name = str(r.get('workflow_name', ''))[:70]
    date = str(r.get('created_at', ''))[:19]
    status = r.get('status', '')
    print(f'  {rid} | {status} | {date} | {name}')

# Check fields
if runs:
    print(f'\nRun fields: {list(runs[0].keys())}')

# Check reflection output
print('\n--- Checking reflection run output ---')
for r in reflections[:2]:
    rid = r.get('id', '')
    print(f'\nReflection: {rid}')
    try:
        out_resp = urllib.request.urlopen(f'http://localhost:9876/task-runs/{rid}/output?tail_chars=1500')
        output = json.loads(out_resp.read().decode())
        text = output.get('data', output) if isinstance(output, dict) else str(output)
        if isinstance(text, dict):
            text = text.get('output', str(text))
        if isinstance(text, str) and len(text) > 500:
            text = '...' + text[-500:]
        print(f'  Output: {text}')
    except Exception as e:
        print(f'  Error: {e}')
