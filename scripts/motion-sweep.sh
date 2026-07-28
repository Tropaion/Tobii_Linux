#!/usr/bin/env bash
# Capture a deliberate head-motion sweep and report whether each axis of the
# normalized trackbox signal actually responds.
#
# WHY THIS EXISTS: the eye-position display is driven by the 0x500 gaze frame's
# trackbox columns (0x03/0x09), whose x/y/z are all normalized [0,1]. If one axis
# barely moves while the head does, the display cannot help but feel dead on that
# axis — no amount of display-side algorithm work can recover it. A short capture
# of someone sitting still looks identical to a broken axis, so this script
# prompts for real movement and reports each axis's range against the eye-origin
# millimetre columns, which are an independent witness to the same motion.
#
# Usage:  ./scripts/motion-sweep.sh [SECONDS]      (default 45)
set -uo pipefail

SECS="${1:-45}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/target/release/tobii"
OUT="$(mktemp -t tobii-motion-XXXXXX.log)"

[ -x "$BIN" ] || { echo "build first: cargo build -p tobii-cli --release"; exit 1; }

cat <<EOF
Motion sweep — ${SECS}s. Sit in front of the tracker, then, taking your time:

  1. Move your head UP and DOWN through the full range you would ever use
  2. Move LEFT and RIGHT across the full width
  3. Lean IN toward the screen, then BACK away

Cover each axis for roughly a third of the time. Starting in 3 seconds...
EOF
sleep 3
echo "capturing..."

timeout "$SECS" "$BIN" stream --eyes > "$OUT" 2>&1

python3 - "$OUT" <<'PY'
import re, statistics as st, sys
lines = open(sys.argv[1]).read().split('\n')
rows = []
for i, l in enumerate(lines):
    if 'valL=0 valR=0' not in l or i + 2 >= len(lines):
        continue
    m = re.search(r'trackbox L=\(([-\d.]+), ([-\d.]+), z=([-\d.]+)\)', lines[i + 1])
    o = re.search(r'origin L=\(([-\d]+), ([-\d]+), ([-\d]+)\)mm', lines[i + 2])
    if m and o:
        rows.append(tuple(float(m.group(k)) for k in (1, 2, 3)) +
                    tuple(int(o.group(k)) for k in (1, 2, 3)))

total = sum(1 for l in lines if l.startswith('t='))
print(f"\nframes: {total} total, {len(rows)} with both eyes valid")
if not rows:
    print("\nNo valid frames. Either nobody was in view, or the tracker lost you")
    print("for the whole window — try again sitting squarely in front of it.")
    raise SystemExit

def rng(vals):
    return max(vals) - min(vals)

nx, ny, nz = (rng([r[i] for r in rows]) for i in range(3))
mx, my, mz = (rng([r[i] for r in rows]) for i in range(3, 6))

print("\n           normalized range      real motion (mm)   normalized per 100mm")
for name, n, mm in (("horizontal", nx, mx), ("vertical  ", ny, my), ("depth     ", nz, mz)):
    gain = (n / mm * 100) if mm > 5 else float('nan')
    flag = ""
    if mm > 40 and n < 0.05:
        flag = "  <-- BARELY RESPONDS"
    print(f"  {name}   {n:6.3f}              {mm:6.0f}            {gain:8.3f}{flag}")

print("\nInterpretation: the third column is how far the normalized signal travels")
print("per 100 mm of real head movement. The three axes should be broadly")
print("comparable. An axis that moved a real distance (>40 mm) while its")
print("normalized value moved <0.05 is compressed or saturated, and that is a")
print("signal-side defect, not a display-side one.")
PY

echo ""
echo "raw capture kept at: $OUT"
