#!/usr/bin/env python3
"""Summarise an rknn-toolkit2 verbose build log: op targets and the compiler's cycle estimate."""
import re
import sys

for path in sys.argv[1:]:
    rows, cpu = [], []
    for line in open(path):
        m = re.match(r'D RKNN: \[[^]]*\] (\d+)\s+(\S+)\s+(\S+)\s+(\S+)\s+.*?\s(\d+)/(\d+)/(\d+)\s+(\d+)', line)
        if not m:
            continue
        _, op, dt, tgt, ddr, npu, tot, rw = m.groups()
        rows.append((op, dt, tgt, int(ddr), int(npu), int(tot), int(rw)))
        if tgt != 'NPU' and op not in ('InputOperator', 'OutputOperator'):
            cpu.append(op)
    tot = sum(r[5] for r in rows)
    rw = sum(r[6] for r in rows) / 1024
    dts = sorted({r[1] for r in rows if r[2] == 'NPU'})
    print(f'{path}: ops={len(rows)} npu-dtypes={dts} non-NPU compute ops={cpu or "none"} '
          f'est.cycles(1 core)={tot / 1e6:.1f}M (~{tot / 1e6:.1f} ms @1GHz) RW={rw:.0f} MB')
