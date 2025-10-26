#!/usr/bin/env python3
"""Fix image references in config by converting embedded base64 to imageId references."""

import json
import sys
import os
import hashlib


def get_image_hash(data):
    """Get a hash of image data for comparison."""
    if data.startswith('data:image'):
        # Extract base64 part after the comma
        base64_data = data.split(',', 1)[1] if ',' in data else data
    else:
        base64_data = data
    return hashlib.md5(base64_data.encode()).hexdigest()


def fix_config_image_refs(config_path, output_path=None):
    """Fix state image references in config."""
    print(f"Loading config: {config_path}\n")

    with open(config_path, 'r') as f:
        config = json.load(f)

    # Build image lookup by hash
    images = config.get('images', [])
    image_hash_map = {}

    print(f"Building image lookup table...")
    for img in images:
        img_data = img.get('data', '')
        if img_data:
            img_hash = get_image_hash(img_data)
            image_hash_map[img_hash] = img.get('id')
            print(f"  {img.get('name')}: {img.get('id')} -> {img_hash[:8]}...")

    print(f"\n✓ Found {len(images)} images in lookup table\n")

    # Fix state images
    states = config.get('states', [])
    fixes_made = 0
    new_images_needed = []

    for state in states:
        state_name = state.get('name', state.get('id'))
        state_images = state.get('stateImages', [])

        if not state_images:
            continue

        print(f"Checking state '{state_name}'...")

        for state_img in state_images:
            img_name = state_img.get('name', 'unnamed')
            patterns = state_img.get('patterns', [])

            for pattern in patterns:
                # Check if pattern has embedded data
                if 'image' in pattern and pattern['image'].startswith('data:image'):
                    embedded_data = pattern['image']
                    embedded_hash = get_image_hash(embedded_data)

                    # Try to find matching image
                    if embedded_hash in image_hash_map:
                        # Found matching image!
                        image_id = image_hash_map[embedded_hash]
                        print(f"  ✓ Fixing '{img_name}': {embedded_hash[:8]}... -> {image_id}")

                        # Replace 'image' with 'imageId'
                        pattern['imageId'] = image_id
                        del pattern['image']
                        fixes_made += 1
                    else:
                        # No matching image found - need to create one
                        new_image_id = f"image-{state_img['id']}-extracted"
                        print(f"  ! Need to create new image for '{img_name}': {new_image_id}")

                        # Add to new images list
                        new_images_needed.append({
                            'id': new_image_id,
                            'name': f"{img_name} (extracted)",
                            'data': embedded_data
                        })

                        # Replace 'image' with 'imageId'
                        pattern['imageId'] = new_image_id
                        del pattern['image']
                        fixes_made += 1

    # Add new images to config
    if new_images_needed:
        print(f"\n✓ Adding {len(new_images_needed)} new images to config")
        config['images'].extend(new_images_needed)

    print(f"\n{'='*60}")
    print(f"✅ Fixed {fixes_made} image references")

    if new_images_needed:
        print(f"✅ Created {len(new_images_needed)} new image entries")

    # Save fixed config
    if output_path is None:
        # Save with _fixed suffix
        base, ext = os.path.splitext(config_path)
        output_path = f"{base}_fixed{ext}"

    with open(output_path, 'w') as f:
        json.dump(config, f, indent=2)

    print(f"\n✅ Saved fixed config to: {output_path}")
    print(f"{'='*60}\n")

    return fixes_made > 0


if __name__ == "__main__":
    config_path = os.path.join(
        os.path.dirname(__file__),
        "..", "..",
        "bdo_config (42).json"
    )

    if not os.path.exists(config_path):
        print(f"Config not found: {config_path}")
        sys.exit(1)

    success = fix_config_image_refs(config_path)

    # Verify the fixed config
    if success:
        fixed_path = config_path.replace('.json', '_fixed.json')
        print("\nVerifying fixed config...")

        # Import and run the diagnostic script
        sys.path.insert(0, os.path.dirname(__file__))
        from test_config_image_refs import check_config_image_refs

        if check_config_image_refs(fixed_path):
            print("\n✅ Fixed config passes validation!")
            sys.exit(0)
        else:
            print("\n❌ Fixed config still has issues")
            sys.exit(1)
    else:
        print("\n❌ No fixes were made")
        sys.exit(1)
