import json
import sys
import argparse
from jsonschema import validate, ValidationError

# JSON Schemas for deploy, mint, and transfer operations
DEPLOY_SCHEMA = {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "title": "BRC-2.0 Deploy Inscription",
    "type": "object",
    "required": ["p", "op", "tick", "max", "dec"],
    "properties": {
        "p": {"type": "string", "const": "brc-2.0"},
        "op": {"type": "string", "enum": ["deploy"]},
        "tick": {
            "type": "string",
            "maxLength": 4,
            "pattern": "^[A-Z0-9]+$"
        },
        "max": {
            "type": "string",
            "pattern": "^[0-9]+$"
        },
        "dec": {
            "type": "integer",
            "minimum": 0,
            "maximum": 18
        },
        "meta": {
            "type": "object",
            "properties": {
                "vesting": {"type": "boolean"},
                "start": {"type": "integer"},
                "cliff": {"type": "integer"},
                "duration": {"type": "integer"}
            },
            "required": ["vesting", "start", "cliff", "duration"],
            "if": {
                "properties": {"vesting": {"const": True}}
            },
            "then": {
                "required": ["start", "cliff", "duration"]
            },
            "additionalProperties": False
        }
    },
    "additionalProperties": False
}

MINT_SCHEMA = {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "title": "BRC-2.0 Mint Inscription",
    "type": "object",
    "required": ["p", "op", "tick", "amt"],
    "properties": {
        "p": {"type": "string", "const": "brc-2.0"},
        "op": {"type": "string", "enum": ["mint"]},
        "tick": {
            "type": "string",
            "maxLength": 4,
            "pattern": "^[A-Z0-9]+$"
        },
        "amt": {
            "type": "string",
            "pattern": "^[0-9]+$"
        }
    },
    "additionalProperties": False
}

TRANSFER_SCHEMA = {
    "$schema": "http://json-schema.org/draft-07/schema#",
    "title": "BRC-2.0 Transfer Inscription",
    "type": "object",
    "required": ["p", "op", "tick", "to", "amt"],
    "properties": {
        "p": {"type": "string", "const": "brc-2.0"},
        "op": {"type": "string", "enum": ["transfer"]},
        "tick": {
            "type": "string",
            "maxLength": 4,
            "pattern": "^[A-Z0-9]+$"
        },
        "to": {
            "type": "string",
            "pattern": "^bc1[a-z0-9]{25,39}$"
        },
        "amt": {
            "type": "string",
            "pattern": "^[0-9]+$"
        }
    },
    "additionalProperties": False
}

def smart_checks(inscription):
    errors = []

    tick = inscription.get("tick", "")
    if len(tick) > 4:
        errors.append(f"Ticker '{tick}' is longer than 4 characters.")
    if not tick.isupper():
        errors.append(f"Ticker '{tick}' should be uppercase.")
    if not tick.isalnum():
        errors.append(f"Ticker '{tick}' should be alphanumeric (A-Z, 0-9) only.")

    op = inscription.get("op")
    if op == "deploy":
        meta = inscription.get("meta", {})
        if meta.get("vesting", False):
            for field in ["start", "cliff", "duration"]:
                if field not in meta:
                    errors.append(f"Vesting meta missing required field '{field}'.")
                else:
                    val = meta[field]
                    if not isinstance(val, int) or val < 0:
                        errors.append(f"Vesting field '{field}' must be a non-negative integer.")

    # Additional smart checks can be added here

    return errors

def main():
    parser = argparse.ArgumentParser(description="BRC-2.0 JSON Schema Validator with Smart Suggestions")
    parser.add_argument("file", nargs="?", help="Path to JSON inscription file. Reads stdin if omitted.")

    args = parser.parse_args()

    try:
        if args.file:
            with open(args.file, "r") as f:
                data = json.load(f)
        else:
            data = json.load(sys.stdin)
    except Exception as e:
        print(f"Error reading JSON: {e}")
        sys.exit(1)

    # Choose schema based on 'op'
    op = data.get("op")
    if op == "deploy":
        schema = DEPLOY_SCHEMA
    elif op == "mint":
        schema = MINT_SCHEMA
    elif op == "transfer":
        schema = TRANSFER_SCHEMA
    else:
        print(f"Error: Unsupported or missing 'op' field: {op}")
        sys.exit(1)

    # Validate JSON schema
    try:
        validate(instance=data, schema=schema)
        print("✅ JSON Schema validation passed.")
    except ValidationError as ve:
        print(f"❌ JSON Schema validation error: {ve.message}")
        sys.exit(1)

    # Run smart checks
    errors = smart_checks(data)
    if errors:
        print("⚠️  Smart validation warnings:")
        for err in errors:
            print(f" - {err}")
    else:
        print("✅ Smart validation passed. No issues detected.")

if __name__ == "__main__":
    main()

