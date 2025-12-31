Sure! Here’s a clean, complete `README.md` for your `brc2.0-validator` repo including the description, usage, and what the tool does:

````markdown
# BRC-2.0 Validator CLI

A Python CLI tool to validate BRC-2.0 token inscriptions (`deploy`, `mint`, `transfer`) against JSON Schema with smart validation suggestions.

## Features

- Validates BRC-2.0 JSON inscriptions for `deploy`, `mint`, and `transfer` operations.
- Provides detailed JSON Schema validation errors.
- Offers smart suggestions and warnings for common mistakes like ticker format and vesting metadata.
- Supports reading JSON from a file or stdin.

## Usage

Validate JSON from a file:

```bash
python3 brc2_validator.py path/to/inscription.json
````

Or via stdin:

```bash
cat inscription.json | python3 brc2_validator.py
```

The tool checks for required fields, formats, and common mistakes and provides actionable warnings.

## Requirements

* Python 3.7+
* [jsonschema](https://pypi.org/project/jsonschema/)

Install dependencies with:

```bash
pip install jsonschema
```

## Example

Here is a sample `deploy` inscription JSON:

```json
{
  "p": "brc-2.0",
  "op": "deploy",
  "tick": "VEST",
  "max": "1000000000",
  "dec": 8,
  "meta": {
    "vesting": true,
    "start": 1720000000,
    "cliff": 1,
    "duration": 6
  }
}
```

Save it as `inscription.json` and validate with:

```bash
python3 brc2_validator.py inscription.json
```

---

## Contributing

Contributions and suggestions are welcome! Feel free to open issues or submit pull requests.

## License

MIT License

