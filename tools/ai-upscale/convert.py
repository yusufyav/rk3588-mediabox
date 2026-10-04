#!/usr/bin/env python3
"""ONNX -> RKNN for rk3588, FP16 or INT8 (asymmetric, calibrated on real Y planes)."""
import argparse
from rknn.api import RKNN

ap = argparse.ArgumentParser()
ap.add_argument('onnx')
ap.add_argument('out')
ap.add_argument('--int8', metavar='DATASET', help='text file, one .npy (1x1xHxW float Y) per line')
ap.add_argument('--algo', default='normal', choices=['normal', 'mmse', 'kl_divergence'])
ap.add_argument('--std', type=float, help='input std: the graph takes codes / STD')
a = ap.parse_args()

r = RKNN(verbose=True)
norm = {}
if a.std:
    norm = dict(mean_values=[[0.0] * 6], std_values=[[a.std] * 6])
r.config(target_platform='rk3588', quantized_dtype='w8a8', quantized_algorithm=a.algo,
         optimization_level=3, **norm)
assert r.load_onnx(a.onnx) == 0
assert r.build(do_quantization=bool(a.int8), dataset=a.int8) == 0
assert r.export_rknn(a.out) == 0
r.release()
