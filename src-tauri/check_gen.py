import urllib.request
import json

def api(path):
    try:
        resp = urllib.request.urlopen(f'http://localhost:9876{path}')
        return json.loads(resp.read().decode())
    except Exception as e:
        return {'error': str(e)}

# Check today's task runs in detail
print("=== Today's task runs (March 4) ===")
result = api('/task-runs')
runs = result.get('data', result) if isinstance(result, dict) else result
today_runs = [r for r in runs if '2026-03-04' in str(r.get('created_at', ''))]
for r in today_runs:
    rid = str(r.get('id', ''))[:12]
    name = str(r.get('workflow_name', ''))[:70]
    wtype = r.get('workflow_type', '?')
    is_ref = r.get('is_reflection', False)
    status = r.get('status', '')
    depth = r.get('depth', '?')
    prompt = str(r.get('prompt', ''))[:100]
    print(f"  {rid} | {status} | type={wtype} | reflection={is_ref} | depth={depth}")
    print(f"    name: {name}")
    print(f"    prompt: {prompt}")
    print()

# Check if there's a generate endpoint or workflow generation endpoint
print("=== Check generation-related endpoints ===")
for path in ['/workflows/generate', '/generate', '/generator-eval/artifacts?limit=3']:
    result = api(path)
    if 'error' in result:
        err = str(result['error'])[:80]
        print(f"  {path}: {err}")
    else:
        data = result.get('data', result) if isinstance(result, dict) else result
        if isinstance(data, list):
            print(f"  {path}: {len(data)} items")
            for item in data[:2]:
                if isinstance(item, dict):
                    desc = str(item.get('description', ''))[:60]
                    date = str(item.get('created_at', ''))[:19]
                    wfid = str(item.get('workflow_id', 'none'))[:12]
                    print(f"    {date} | wf={wfid} | {desc}")

# Check the specific workflow IDs from the task runs
print("\n=== Checking workflow IDs from today's runs ===")
for r in today_runs:
    wf_name = r.get('workflow_name', '')
    if 'Reflection' not in wf_name:
        # These are the generated+executed workflows
        print(f"  Run: {r['id'][:12]} - {wf_name[:60]}")
        # Check if this workflow has an artifact
        # Try to find by description match
        result = api('/generator-eval/artifacts?limit=50')
        if 'error' not in result:
            artifacts = result.get('data', [])
            matching = [a for a in artifacts if
                        wf_name.replace('AI Generate: ', '') in str(a.get('description', ''))]
            if matching:
                print(f"    Found matching artifact: {matching[0]['id'][:12]} from {matching[0]['created_at'][:19]}")
            else:
                print(f"    No matching artifact found")
                # Check by workflow_id
                wf_artifacts = [a for a in artifacts if a.get('workflow_id') and
                               str(a.get('workflow_id', ''))[:8] == r['id'][:8]]
                if wf_artifacts:
                    print(f"    Found by workflow_id: {wf_artifacts[0]['id'][:12]}")
