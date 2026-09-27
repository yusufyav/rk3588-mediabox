#!/usr/bin/env python3
"""Decode ATV Voice-over-BLE audio frames (ab5e0003) from a btmon text dump."""
import re, sys, struct, wave, math

STEP = [7,8,9,10,11,12,13,14,16,17,19,21,23,25,28,31,34,37,41,45,50,55,60,66,73,80,88,97,107,118,130,143,157,173,190,209,230,253,279,307,337,371,408,449,494,544,598,658,724,796,876,963,1060,1166,1282,1411,1552,1707,1878,2066,2272,2499,2749,3024,3327,3660,4026,4428,4871,5358,5894,6484,7132,7845,8630,9493,10442,11487,12635,13899,15289,16818,18500,20350,22385,24623,27086,29794,32767]
INDEX = [-1,-1,-1,-1,2,4,6,8]

def decode(frame, high_first):
    pred = struct.unpack(">h", frame[3:5])[0]
    index = frame[5]
    if index > 88:
        return None
    out = []
    for byte in frame[6:]:
        for nib in ((byte >> 4, byte & 15) if high_first else (byte & 15, byte >> 4)):
            step = STEP[index]
            diff = step >> 3
            if nib & 4: diff += step
            if nib & 2: diff += step >> 1
            if nib & 1: diff += step >> 2
            pred = pred - diff if nib & 8 else pred + diff
            pred = max(-32768, min(32767, pred))
            index = max(0, min(88, index + INDEX[nib & 7]))
            out.append(pred)
    return out

frames = []
lines = open(sys.argv[1]).read().splitlines()
for i, l in enumerate(lines):
    if "Handle Value Notification" in l and "0x003d" in lines[i+1]:
        chunk = " ".join(lines[i+1:i+12])
        m = re.search(r"Data\[134\]: ([0-9a-f]+)", chunk)
        if m:
            frames.append(bytes.fromhex(m.group(1)))
print("frames", len(frames), "sequence ok:", [struct.unpack(">H", f[:2])[0] for f in frames[:5]], "...")
seqs = [struct.unpack(">H", f[:2])[0] for f in frames]
gaps = sum(1 for a, b in zip(seqs, seqs[1:]) if b != (a + 1) & 0xffff)
print("sequence gaps:", gaps)
for high in (True, False):
    pcm = []
    for f in frames:
        pcm += decode(f, high) or []
    rms = math.sqrt(sum(x*x for x in pcm) / len(pcm))
    zc = sum(1 for a, b in zip(pcm, pcm[1:]) if (a < 0) != (b < 0)) / len(pcm)
    clip = sum(1 for x in pcm if abs(x) >= 32767) / len(pcm)
    print(f"nibble {'high' if high else 'low'}-first: samples={len(pcm)} ({len(pcm)/8000:.2f}s @8k) rms={rms:.0f} zero-cross/sample={zc:.3f} clipped={clip:.4f}")
    name = sys.argv[2] + ("-hi.wav" if high else "-lo.wav")
    w = wave.open(name, "wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(8000)
    w.writeframes(struct.pack("<%dh" % len(pcm), *pcm)); w.close()
# energy envelope per 0.25 s for the high-first decode
pcm = []
for f in frames: pcm += decode(f, True)
env = [int(math.sqrt(sum(x*x for x in pcm[i:i+2000]) / 2000)) for i in range(0, len(pcm), 2000)]
print("rms per 0.25s:", env)
