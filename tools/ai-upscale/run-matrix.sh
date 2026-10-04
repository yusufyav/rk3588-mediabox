#!/bin/sh
# Every model in m/ on one NPU core and on all three, CPU stages on the A76s.
# run-matrix.sh SRC.nv12 [iters] > matrix.txt
cd "$(dirname "$0")"
export LD_LIBRARY_PATH=$PWD/lib
src=$1 n=${2:-300}
for m in m/*.rknn; do
	v=${m#*_x2-}; v=${v%%-*}
	for mask in 1 7; do
		taskset -c 4-7 ./aisr-bench "$m" "$v" "$mask" "$src" -w 20 -n "$n" -t 4 2>&1 | grep -vE '^(W|I) '
	done
done
