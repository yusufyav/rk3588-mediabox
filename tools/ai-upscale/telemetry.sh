#!/bin/sh
# One line per second until killed: NPU load/freq, DDR freq/load, GPU load,
# CPU busy %, A76 frequencies, temperatures. telemetry.sh > tele.csv
dev=/sys/class/devfreq
echo "t,npu_load,npu_mhz,ddr_mhz,ddr_load,gpu_load,cpu_busy,a76_mhz_4,a76_mhz_6,soc_c,big0_c,big1_c,npu_c,gpu_c"
prev=$(awk '/^cpu /{print $2+$3+$4+$5+$6+$7+$8, $5+$6}' /proc/stat)
while :; do
	sleep 1
	cur=$(awk '/^cpu /{print $2+$3+$4+$5+$6+$7+$8, $5+$6}' /proc/stat)
	busy=$(echo "$prev $cur" | awk '{dt=$3-$1; di=$4-$2; printf "%.1f", dt ? 100*(dt-di)/dt : 0}')
	prev=$cur
	load=$(sed 's/.*Core0: *\([0-9]*\)%, Core1: *\([0-9]*\)%, Core2: *\([0-9]*\)%.*/\1|\2|\3/' /sys/kernel/debug/rknpu/load)
	t() { echo $(( $(cat /sys/class/thermal/thermal_zone$1/temp) / 100 )) | sed 's/\(.\)$/.\1/'; }
	echo "$(date +%s),$load,$(( $(cat $dev/fdab0000.npu/cur_freq) / 1000000 )),$(( $(cat $dev/dmc/cur_freq) / 1000000 )),$(cut -d@ -f1 $dev/dmc/load),$(cut -d@ -f1 $dev/fb000000.gpu/load),$busy,$(( $(cat /sys/devices/system/cpu/cpufreq/policy4/scaling_cur_freq) / 1000 )),$(( $(cat /sys/devices/system/cpu/cpufreq/policy6/scaling_cur_freq) / 1000 )),$(t 0),$(t 1),$(t 2),$(t 6),$(t 5)"
done
