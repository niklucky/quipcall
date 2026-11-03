#!/bin/bash
# Sample a process every 5 s: CPU %, RSS, physical footprint, IOSurface count.
#
#   dev/measure.sh $(pgrep -x quipcall-desktop) 8
#
# Use the footprint, not RSS: it is what Activity Monitor shows as "Memory" and
# includes GPU and IOSurface memory, which RSS mostly misses.
pid=${1:?usage: measure.sh PID [SAMPLES]}
for _ in $(seq 1 "${2:-6}"); do
  cpu_rss=$(ps -o %cpu=,rss= -p "$pid" | awk '{printf "cpu=%s%% rss=%dMB", $1, $2/1024}')
  vm=$(vmmap --summary "$pid" 2>/dev/null)
  footprint=$(echo "$vm" | awk -F: '/^Physical footprint:/ {gsub(/ /, "", $2); print $2}')
  iosurfaces=$(echo "$vm" | awk '$1 == "IOSurface" {print $NF}')
  echo "$cpu_rss footprint=$footprint iosurfaces=${iosurfaces:-0}"
  sleep 5
done
