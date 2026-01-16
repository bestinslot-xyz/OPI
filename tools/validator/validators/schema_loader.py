# validators/schema_loader.py

import json
import os

DEFAULT_SCHEMA = {
    "type": "object",
    "required": ["p", "op", "tick"],
    "properties": {
        "p": {"type": "string", "const": "brc-2.0"},
        "op": {"type": "string", "enum": ["deploy", "mint", "transfer"]},
        "tick": {"type": "string", "maxLength": 4},
        "amt": {"type": "string"},
        "max": {"type": "string"},
        "dec": {"type": "integer"},
        "meta": {"type": "object"}
    }
}

def load_schema(schema_path=None):
    if not schema_path:
        return DEFAULT_SCHEMA

    if not os.path.exists(schema_path):
        print("❌ Custom schema not found. Using default.")
        return DEFAULT_SCHEMA

    try:
        with open(schema_path, 'r') as f:
            return json.load(f)
    except Exception as e:
        print(f"❌ Failed to load custom schema: {e}")
        return DEFAULT_SCHEMA

