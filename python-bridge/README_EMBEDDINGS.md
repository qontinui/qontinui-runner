# RAG Embedding Generation Script

This directory contains the Python script for generating embeddings for Qontinui RAG projects.

## Overview

The `generate_embeddings.py` script is called by the Rust runner to generate multimodal embeddings for GUI elements. It uses the qontinui RAG module to create text, CLIP, and DINOv2 embeddings and store them in a local Qdrant vector database.

## Requirements

The script requires the `rag` extras for the qontinui package:

```bash
poetry install
```

This installs:

- `sentence-transformers` - For text embeddings
- `transformers` - For CLIP embeddings
- `torch` - For deep learning models
- `qdrant-client` - For vector database storage

## Usage

### Command Line

```bash
python generate_embeddings.py --project-id <project-id>
```

### Expected Directory Structure

```
~/.qontinui/rag/{project_id}/
├── config.json              # RAG configuration with GUI elements
├── screenshots/             # Screenshot images
│   ├── screenshot1.png
│   └── screenshot2.png
└── embeddings/             # Generated embeddings (created by script)
    ├── vector.qvdb/        # Qdrant vector database
    └── embeddings.json     # Embedding metadata
```

### Config Format

The `config.json` file should contain an array of GUI elements:

```json
{
  "elements": [
    {
      "id": "elem1",
      "source_app": "MyApp",
      "source_screenshot_id": "screenshot1",
      "bounding_box": {
        "x": 100,
        "y": 200,
        "width": 120,
        "height": 40
      },
      "element_type": "button",
      "ocr_text": "Sign In",
      "text_description": "",
      "element_subtype": "primary",
      "is_interactive": true
    }
  ]
}
```

## Output

The script outputs progress as JSON lines to stdout:

```json
{"status": "progress", "percent": 50, "message": "Generating embeddings..."}
{"status": "complete", "elements_embedded": 42, "total": 42, "failed": 0}
{"status": "error", "message": "File not found: config.json"}
```

### Progress Events

- `progress` - Incremental progress update with percent and message
- `complete` - Successful completion with summary
- `error` - Fatal error with message

## Embeddings Generated

For each GUI element, the script generates:

1. **Text Embedding** (384-dim)
   - Uses `all-MiniLM-L6-v2` sentence-transformers model
   - Generates semantic description from element metadata
   - Enables text-based search

2. **CLIP Embedding** (512-dim)
   - Uses `openai/clip-vit-base-patch32` model
   - Crops element region from screenshot
   - Enables visual similarity search

3. **DINOv2 Embedding** (768-dim)
   - Uses `dinov2_vitb14` model
   - Crops element region from screenshot
   - Enables fine-grained visual feature matching

## Vector Database

Embeddings are stored in a local Qdrant database at `~/.qontinui/rag/{project_id}/embeddings/vector.qvdb/`.

The database uses multi-vector configuration:

- `text_embedding` - 384-dim, Cosine distance
- `clip_embedding` - 512-dim, Cosine distance
- `dinov2_embedding` - 768-dim, Cosine distance

## Error Handling

The script handles errors gracefully:

- Missing config file → Fatal error
- Missing screenshots → Fatal error
- Missing bounding box → Skip element, continue
- Embedding generation failure → Skip element, log warning

Failed elements are tracked in `embeddings.json` under the `errors` field.

## Integration with Rust Runner

The Rust runner should:

1. Call script: `python generate_embeddings.py --project-id {id}`
2. Parse JSON lines from stdout
3. Update progress UI based on `percent` field
4. Handle completion/error status

Example Rust code:

```rust
let output = Command::new("python")
    .arg("generate_embeddings.py")
    .arg("--project-id")
    .arg(project_id)
    .stdout(Stdio::piped())
    .spawn()?;

let reader = BufReader::new(output.stdout);
for line in reader.lines() {
    let json: serde_json::Value = serde_json::from_str(&line?)?;
    match json["status"].as_str() {
        Some("progress") => update_progress(json["percent"].as_u64()),
        Some("complete") => handle_completion(json),
        Some("error") => handle_error(json["message"].as_str()),
        _ => {}
    }
}
```

## Development

To test the script locally:

```bash
# Create test project
mkdir -p ~/.qontinui/rag/test-project/screenshots

# Create test config
cat > ~/.qontinui/rag/test-project/config.json << EOF
{
  "elements": [
    {
      "id": "test1",
      "source_screenshot_id": "screenshot1",
      "bounding_box": {"x": 100, "y": 100, "width": 200, "height": 50},
      "element_type": "button",
      "ocr_text": "Click Me"
    }
  ]
}
EOF

# Add screenshot
cp /path/to/screenshot.png ~/.qontinui/rag/test-project/screenshots/screenshot1.png

# Run script
python generate_embeddings.py --project-id test-project
```

## Troubleshooting

### Import Errors

If you get `ImportError: sentence-transformers is not installed`:

```bash
cd /path/to/qontinui
poetry install -E rag
```

### CUDA Out of Memory

If running on GPU with limited memory, the models will automatically fall back to CPU. You can force CPU mode by setting:

```bash
export CUDA_VISIBLE_DEVICES=""
```

### Model Download Issues

Models are downloaded from HuggingFace on first use. If downloads fail, check internet connection or set a cache directory:

```bash
export TRANSFORMERS_CACHE=/path/to/cache
```
