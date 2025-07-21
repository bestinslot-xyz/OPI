# validators/gating.py

def validate_gating(meta):
    gating = meta.get("token_gating")
    if not gating:
        return

    errors = []
    if not gating.get("required_token"):
        errors.append("Missing 'required_token'")
    if not isinstance(gating.get("min_balance", 0), (int, float)):
        errors.append("'min_balance' should be a number")
    if "expiry" in gating and gating["expiry"] <= 0:
        errors.append("'expiry' must be a valid future UNIX timestamp")

    if errors:
        print("❌ Token gating validation errors:")
        for err in errors:
            print(f"  - {err}")
    else:
        print("✅ Token gating rules validated.")

