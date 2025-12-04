# brc2.0_validator.py

import json
import argparse
from jsonschema import validate, ValidationError
from validators.vesting import simulate_vesting
from validators.gating import validate_gating
from validators.schema_loader import load_schema

def main():
    parser = argparse.ArgumentParser(description="BRC-2.0 JSON Validator CLI with Advanced Features")
    parser.add_argument("json_file", help="Path to BRC-2.0 inscription JSON file")
    parser.add_argument("--schema", help="Optional custom schema file path")
    parser.add_argument("--timestamp", type=int, help="Optional UNIX timestamp to simulate vesting")
    args = parser.parse_args()

    # Load JSON inscription
    try:
        with open(args.json_file, 'r') as f:
            data = json.load(f)
    except Exception as e:
        print(f"❌ Failed to load JSON: {e}")
        return

    # Load schema
    schema = load_schema(args.schema)

    # Validate JSON against schema
    try:
        validate(instance=data, schema=schema)
        print("✅ JSON schema validation passed.")
    except ValidationError as ve:
        print("❌ Schema validation error:")
        print(ve.message)
        return

    # Advanced checks
    if data.get("meta"):
        simulate_vesting(data["meta"], args.timestamp)
        validate_gating(data["meta"])

if __name__ == "__main__":
    main()

