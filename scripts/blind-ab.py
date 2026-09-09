#!/usr/bin/env python3
"""Blind, interleaved A/B runner for Gate MP1b-FINAL.

Gate MP1b-CSC proved the mechanical half of the colour-encoding question --
requesting ITU-R BT.2020 YCbCr on the scanout plane is accepted, reads back,
and makes VOP2 load a different CSC matrix -- and failed to answer the
perceptual half. It failed for a protocol reason: two 120-second blocks four
minutes apart is not an instrument that can resolve a matrix error on muted
material. Three things were wrong with it and this runner fixes all three:

  * the operator knew which leg was which, so the comparison was not blind;
  * the legs were minutes apart, so it tested long-term colour memory; and
  * the scene was the muted 20:00 one, where the effect under test is close
    to its minimum.

So: short trials, interleaved in counterbalanced pairs, in a randomised order
the operator cannot predict, on a scene chosen for chroma magnitude. The
variant mapping is written to a file this script does not print, so neither
the operator nor whoever is driving it sees the assignment until `reveal`.

Usage:
    blind-ab.py plan   --out DIR --pairs 8 [--seed N] [--duration 25] [--start 5410]
    blind-ab.py run    --out DIR --pair N
    blind-ab.py score  --out DIR --pair N --choice 1|2|same|unsure
    blind-ab.py reveal --out DIR

`run` prints nothing that identifies the variant. Trial evidence filenames are
numbered by position in the pair, never by variant.
"""

import argparse
import json
import os
import random
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(os.path.dirname(__file__)))

# The two legs. "A" is Gate MP1b's published behaviour: the property is left
# alone and VOP2 tags the window BT.601. "B" is the one-property change.
VARIANTS = {
    "A": "default",
    "B": "bt2020-ycc",
}

SSH = ["ssh", "-F", "/dev/null", "-i", os.path.expanduser("~/.ssh/id_ed25519"),
       "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
       "root@10.27.27.25"]
PROBE = "/tmp/rk3588-mediabox/build/hdr-playback-probe"
SUMMARY = "/sys/kernel/debug/dri/0/summary"


def ssh(cmd, timeout=120):
    return subprocess.run(SSH + [cmd], capture_output=True, text=True, timeout=timeout)


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


def plan(args):
    os.makedirs(args.out, exist_ok=True)
    rng = random.Random(args.seed)
    # Counterbalanced: half the pairs present A first, half present B first,
    # then the order of the pairs themselves is shuffled. A operator who
    # notices "it alternates" still cannot infer which leg they are watching.
    half = args.pairs // 2
    orders = ["AB"] * half + ["BA"] * (args.pairs - half)
    rng.shuffle(orders)
    secret = {
        "seed": args.seed,
        "pairs": args.pairs,
        "duration": args.duration,
        "start": args.start,
        "orders": orders,
        "asset": args.asset,
        "plane": args.plane,
    }
    write(os.path.join(args.out, "mapping.secret.json"), json.dumps(secret, indent=2))
    write(os.path.join(args.out, "scores.jsonl"), "")
    # What is safe to show: everything except the assignment.
    print(f"planned {args.pairs} pairs, {args.duration}s per trial, "
          f"start={args.start}s, seed={args.seed}")
    print("variant assignment written to mapping.secret.json and NOT printed")


def load(args):
    with open(os.path.join(args.out, "mapping.secret.json")) as f:
        return json.load(f)


def run_trial(cfg, out, pair, slot, variant):
    """One trial. Returns nothing that identifies the variant to the caller."""
    tag = f"pair{pair}-trial{slot}"
    enc = VARIANTS[variant]
    dur = cfg["duration"]

    ssh("dmesg > /tmp/blind-dmesg-before.txt")
    cmd = (f"'{PROBE}' --input '{cfg['asset']}' --duration {dur} "
           f"--start {cfg['start']} --plane {cfg['plane']} "
           f"--plane-color-encoding {enc}")
    proc = subprocess.Popen(SSH + [cmd], stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)

    # Sample the pipeline while the content under test is actually on the wire.
    time.sleep(min(10, max(4, dur // 2)))
    dbg = ssh(f"cat {SUMMARY}").stdout
    write(os.path.join(out, f"{tag}-20-summary-during.txt"), dbg)
    tv = subprocess.run([os.path.join(HERE, "scripts/tv-state.sh"), tag],
                        capture_output=True, text=True, timeout=60)
    write(os.path.join(out, f"{tag}-21-tv-state-during.json"), tv.stdout)

    playback = proc.communicate(timeout=dur + 180)[0]
    write(os.path.join(out, f"{tag}-10-playback.txt"), playback)

    delta = ssh("dmesg > /tmp/blind-dmesg-after.txt; "
                "diff /tmp/blind-dmesg-before.txt /tmp/blind-dmesg-after.txt "
                "| sed -n 's/^> //p'").stdout
    write(os.path.join(out, f"{tag}-30-dmesg-delta.txt"), delta)

    # Cleanup, then prove the link really went back to SDR before the next
    # trial starts: an incomplete reset would carry state across the A/B.
    reset = ssh(f"'{PROBE}' --reset").stdout
    write(os.path.join(out, f"{tag}-40-reset.txt"), reset)
    after = ssh(f"cat {SUMMARY} | head -5").stdout
    write(os.path.join(out, f"{tag}-41-summary-after-reset.txt"), after)
    sdr_ok = "SDR[0]" in after and "HDR10" not in after

    result = [l for l in playback.splitlines() if l.startswith("PLAYBACK RESULT:")]
    return {
        "trial": tag,
        "result": result[-1].split(": ")[1] if result else "NO-RESULT-LINE",
        "sdr_reset_verified": sdr_ok,
        "dmesg_delta_lines": len(delta.splitlines()),
    }


def run(args):
    cfg = load(args)
    order = cfg["orders"][args.pair - 1]
    status = []
    for slot, variant in enumerate(order, start=1):
        print(f"  trial {slot} of pair {args.pair}: playing {cfg['duration']}s ...",
              flush=True)
        st = run_trial(cfg, args.out, args.pair, slot, variant)
        # Deliberately does not print the variant.
        print(f"  trial {slot} done: {st['result']}, "
              f"sdr_reset_verified={st['sdr_reset_verified']}, "
              f"dmesg_delta={st['dmesg_delta_lines']} lines", flush=True)
        status.append(st)
    with open(os.path.join(args.out, "trials.jsonl"), "a") as f:
        for st in status:
            f.write(json.dumps(st) + "\n")


def score(args):
    with open(os.path.join(args.out, "scores.jsonl"), "a") as f:
        f.write(json.dumps({"pair": args.pair, "choice": args.choice}) + "\n")
    print(f"recorded pair {args.pair}: {args.choice}")


def reveal(args):
    cfg = load(args)
    scores = {}
    with open(os.path.join(args.out, "scores.jsonl")) as f:
        for line in f:
            if line.strip():
                d = json.loads(line)
                scores[d["pair"]] = d["choice"]

    rows, chose_b, chose_a, decided, same, unsure = [], 0, 0, 0, 0, 0
    for i, order in enumerate(cfg["orders"], start=1):
        choice = scores.get(i, "-")
        picked = "-"
        if choice in ("1", "2"):
            picked = order[int(choice) - 1]
            decided += 1
            if picked == "B":
                chose_b += 1
            else:
                chose_a += 1
        elif choice == "same":
            same += 1
        elif choice == "unsure":
            unsure += 1
        rows.append((i, order, choice, picked))

    out = []
    out.append(f"seed={cfg['seed']} pairs={cfg['pairs']} duration={cfg['duration']}s "
               f"start={cfg['start']}s plane={cfg['plane']}")
    out.append("A = plane COLOR_ENCODING default (VOP2 tags the window BT.601)")
    out.append("B = plane COLOR_ENCODING ITU-R BT.2020 YCbCr")
    out.append("")
    out.append(f"{'pair':>4} {'order':>6} {'operator':>9} {'picked':>7}")
    out.append("-" * 30)
    for i, order, choice, picked in rows:
        out.append(f"{i:>4} {order:>6} {choice:>9} {picked:>7}")
    out.append("")
    out.append(f"decided pairs        : {decided}")
    out.append(f"  picked B (BT.2020) : {chose_b}")
    out.append(f"  picked A (default) : {chose_a}")
    out.append(f"reported 'same'      : {same}")
    out.append(f"reported 'unsure'    : {unsure}")
    if decided:
        # Two-sided exact binomial against p=0.5. With this many trials only a
        # near-sweep can clear 0.05, which is the point: the gate should not be
        # allowed to call a 5-3 split a confirmed effect.
        from math import comb
        n, k = decided, max(chose_b, chose_a)
        p = sum(comb(n, j) for j in range(k, n + 1)) / 2 ** n * 2
        p = min(1.0, p)
        out.append(f"two-sided exact binomial p (vs chance) = {p:.4f}")
    else:
        out.append("two-sided exact binomial p = n/a (no decided pairs)")
    text = "\n".join(out)
    write(os.path.join(args.out, "50-blind-result.txt"), text + "\n")
    print(text)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("plan", "run", "score", "reveal"):
        p = sub.add_parser(name)
        p.add_argument("--out", required=True)
        if name == "plan":
            p.add_argument("--pairs", type=int, default=8)
            p.add_argument("--seed", type=int, default=None)
            p.add_argument("--duration", type=int, default=25)
            p.add_argument("--start", type=int, default=5410)
            p.add_argument("--asset", default="/var/tmp/mp1b/past-lives.mkv")
            p.add_argument("--plane", type=int, default=73)
        if name in ("run", "score"):
            p.add_argument("--pair", type=int, required=True)
        if name == "score":
            p.add_argument("--choice", required=True,
                           choices=["1", "2", "same", "unsure"])
    args = ap.parse_args()
    if args.cmd == "plan" and args.seed is None:
        args.seed = random.SystemRandom().randrange(1 << 30)
    {"plan": plan, "run": run, "score": score, "reveal": reveal}[args.cmd](args)


if __name__ == "__main__":
    main()
