import urllib.request
import json

def api(path):
    try:
        resp = urllib.request.urlopen(f'http://localhost:9876{path}')
        return json.loads(resp.read().decode())
    except Exception as e:
        return {'error': str(e)}

# Check generation rules (reflection fixes get promoted to rules)
print("=== Generation Rules ===")
result = api('/generator-eval/rules')
if 'error' in result:
    print(f"  Error: {result['error']}")
else:
    rules = result.get('data', result) if isinstance(result, dict) else result
    if isinstance(rules, list):
        print(f"  Total rules: {len(rules)}")
        for r in rules[:10]:
            if isinstance(r, dict):
                agent = r.get('agent', '?')
                section = r.get('section', '?')
                content = str(r.get('content', ''))[:100]
                source = r.get('source', '?')
                created = str(r.get('created_at', ''))[:19]
                print(f"  [{agent}/{section}] ({source}) {created}: {content}")
    else:
        print(f"  Response: {str(rules)[:200]}")

# Check reflection effectiveness data (existing endpoint)
print("\n=== Reflection Effectiveness ===")
result = api('/reflection/effectiveness')
if 'error' in result:
    # Try alternate paths
    for path in ['/api/reflection/effectiveness', '/reflection/stats', '/generator-eval/reflection-stats']:
        result = api(path)
        if 'error' not in result:
            print(f"  Found at {path}")
            break
    if 'error' in result:
        print(f"  Not available via API")

# Check the full artifact detail for one March 3 artifact
print("\n=== Artifact Detail (March 3 - fd8df5b0) ===")
result = api('/generator-eval/artifacts/fd8df5b0-15dc-489f-9171-66395c5c87a4')
if 'error' in result:
    # The detail endpoint might not exist
    print(f"  404: artifact detail endpoint not available")
    # Try the list endpoint with more detail
    result = api('/generator-eval/artifacts?limit=1')
    if 'error' not in result:
        data = result.get('data', [])
        if data:
            print(f"  List fields: {list(data[0].keys())}")
else:
    artifact = result.get('data', result) if isinstance(result, dict) else result
    if isinstance(artifact, dict):
        for key in ['specification_prompt', 'builder_prompt', 'hardener_prompt', 'specification_criteria', 'builder_raw_output']:
            val = artifact.get(key)
            if val:
                print(f"  {key}: {str(val)[:80]}...")
            else:
                print(f"  {key}: (null/missing)")

# Check what endpoints the runner actually exposes
print("\n=== Available generator-eval endpoints ===")
for endpoint in ['artifacts', 'insights', 'training-data', 'rules', 'benchmarks', 'stats', 'edit-analysis']:
    result = api(f'/generator-eval/{endpoint}')
    if 'error' in result and '404' in str(result['error']):
        print(f"  /generator-eval/{endpoint}: 404")
    elif 'error' in result:
        err = str(result['error'])[:60]
        print(f"  /generator-eval/{endpoint}: {err}")
    else:
        data = result.get('data', result) if isinstance(result, dict) else result
        count = len(data) if isinstance(data, list) else '?'
        print(f"  /generator-eval/{endpoint}: OK ({count} items)")
