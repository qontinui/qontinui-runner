#!/usr/bin/env python3
"""Diagnose image reference issues in config."""

import json
import sys
import os

def check_config_image_refs(config_path):
    """Check if state images reference valid image IDs."""
    print(f"Checking config: {config_path}\n")

    with open(config_path, 'r') as f:
        config = json.load(f)

    # Get all image IDs
    images = config.get('images', [])
    if isinstance(images, list):
        image_ids = {img.get('id') for img in images if 'id' in img}
    else:
        image_ids = set(images.keys()) if isinstance(images, dict) else set()

    print(f"✓ Found {len(image_ids)} images in config")
    print(f"  Image IDs: {sorted(list(image_ids))[:5]}...\n")

    # Check states
    states = config.get('states', [])
    print(f"✓ Found {len(states)} states\n")

    issues = []

    for state in states:
        state_name = state.get('name', state.get('id'))
        # Try both field names for compatibility
        state_images = state.get('stateImages', state.get('identifyingImages', []))

        if not state_images:
            print(f"  State '{state_name}': No state images")
            continue

        print(f"  State '{state_name}': {len(state_images)} state images")

        for idx, state_img in enumerate(state_images):
            img_id = state_img.get('id')
            img_name = state_img.get('name', 'unnamed')

            # Check if this references an actual image ID
            patterns = state_img.get('patterns', [])
            if patterns:
                for pattern in patterns:
                    pattern_img_id = pattern.get('imageId') or pattern.get('image')

                    if not pattern_img_id:
                        # Check if pattern has embedded base64 data
                        if 'data' in pattern and pattern['data'].startswith('data:image'):
                            issue = f"    ✗ State image '{img_name}' (id={img_id}) has embedded base64 data instead of imageId reference"
                            issues.append(issue)
                            print(issue)
                        else:
                            issue = f"    ✗ State image '{img_name}' (id={img_id}) has no imageId reference"
                            issues.append(issue)
                            print(issue)
                    elif pattern_img_id not in image_ids:
                        issue = f"    ✗ State image '{img_name}' references missing image: {pattern_img_id}"
                        issues.append(issue)
                        print(issue)
                    else:
                        print(f"    ✓ State image '{img_name}' correctly references image: {pattern_img_id}")

    print(f"\n{'='*60}")
    if issues:
        print(f"❌ Found {len(issues)} issues:")
        for issue in issues:
            print(issue)
        print(f"\n{'='*60}")
        print("SOLUTION:")
        print("The state images need to reference image IDs from the 'images' array,")
        print("not embed base64 data directly.")
        print("\nYou need to re-export this config from qontinui-web to fix these")
        print("image references. The frontend export should create proper references.")
        return False
    else:
        print("✅ All state image references are valid!")
        return True

if __name__ == "__main__":
    config_path = os.path.join(
        os.path.dirname(__file__),
        "..", "..",
        "bdo_config (42).json"
    )

    if not os.path.exists(config_path):
        print(f"Config not found: {config_path}")
        sys.exit(1)

    success = check_config_image_refs(config_path)
    sys.exit(0 if success else 1)
