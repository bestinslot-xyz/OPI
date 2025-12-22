**BRC2.0 Validator CLI Tool**

**A Python-powered validator and guide for working with BRC-2.0 JSON inscriptions — complete with smart suggestions, vesting simulation, and token gating checks.**

---

## 📌 Overview

**BRC-2.0** is the next-generation token protocol built on Bitcoin’s Ordinals and Taproot architecture. It enables programmable token logic such as:

- ✅ Vesting schedules
- ✅ Token gating
- ✅ Advanced deploy/mint/transfer operations

This tool helps developers build, test, and debug BRC-2.0 inscriptions before inscribing them — so errors are caught early and behavior is predictable.

---

## ⚙️ Features

- 🔍 **Schema validation** for deploy, mint, and transfer inscriptions
- ⏳ **Vesting simulation** to preview how tokens unlock over time
- 🔐 **Token gating validation** (check access rules like required token balance)
- ⚡ Extensible for custom rules and future BRC-2.0 upgrades

---

## 🚀 Getting Started

```bash
git clone https://github.com/Phantagyro/brc2.0-validator.git
cd brc2.0-validator
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
````

---

## 🧪 Example Usage

Create a JSON file named `inscription.json`:

```json
{
  "p": "brc-2.0",
  "op": "deploy",
  "tick": "TEST",
  "max": "1000000000",
  "dec": 8,
  "meta": {
    "vesting": true,
    "start": 1720000000,
    "cliff": 2,
    "duration": 12,
    "token_gating": {
      "required_token": "GATE",
      "min_balance": 1000,
      "expiry": 2000000000
    }
  }
}
```

Run the validator:

```bash
python3 brc2.0_validator.py inscription.json --timestamp 1750000000
```

Example output:

```
✅ JSON schema validation passed.
🔓 Vesting Simulation at 1750000000: 30% unlocked
✅ Token gating rules validated.
```

---

## 🧰 Requirements

* Python 3.8+
* jsonschema

To install:

```bash
pip install -r requirements.txt
```

Contents of `requirements.txt`:

```
jsonschema
```

---

## 🙌 Contributing

This tool is open source and evolving. Pull requests are welcome!

* Suggest new features
* Add support for new BRC-2.0 mechanics
* Help improve developer tooling for Bitcoin’s smart inscription layer

---

## 📬 Community & Credits

Shoutout to the BRC2.0 and Ordinals ecosystem — especially builders in Fractal, Unisat, and BitOS.

Share your tools and thoughts with:

```
#BRC20 #BRC2 #OrdinalsDev
```

---

## 📄 License

This project is open-source and released under the [MIT License](LICENSE).

---

**Maintainer:** [Phantagyro](https://github.com/Phantagyro)
**Last Updated:** July 2025

