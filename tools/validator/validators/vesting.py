# validators/vesting.py

import time

def simulate_vesting(meta, timestamp=None):
    if not meta.get("vesting"):
        return

    start = meta.get("start")
    cliff = meta.get("cliff", 0)
    duration = meta.get("duration")

    if not all([start, duration]):
        print("⚠️ Vesting metadata incomplete.")
        return

    now = timestamp or int(time.time())
    elapsed_months = max(0, (now - start) // (30 * 24 * 3600))

    if elapsed_months < cliff:
        unlocked = 0
    elif elapsed_months >= duration:
        unlocked = 100
    else:
        unlocked = int((elapsed_months - cliff) / (duration - cliff) * 100)

    print(f"🔓 Vesting Simulation at {now}: {unlocked}% unlocked")

