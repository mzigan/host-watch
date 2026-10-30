LC_ALL=C
export LC_ALL
printf 'hostname=%s\nkernel=%s\n' "$(hostname)" "$(uname -r)"
awk '{print "uptime_seconds=" $1}' /proc/uptime
awk '/^cpu / {idle=$5+$6; total=0; for(i=2;i<=9;i++) total+=$i; printf "cpu_total=%.0f\ncpu_idle=%.0f\n",total,idle} /^cpu[0-9]+ / {n++} END {print "cpu_count=" n}' /proc/stat
awk '{print "load1=" $1 "\nload5=" $2 "\nload15=" $3}' /proc/loadavg
awk '/^MemTotal:/ {print "memory_total_kb=" $2} /^MemAvailable:/ {print "memory_available_kb=" $2} /^SwapTotal:/ {print "swap_total_kb=" $2} /^SwapFree:/ {print "swap_free_kb=" $2}' /proc/meminfo
# Explicit local filesystem allowlist excludes virtual and network mounts.
df -Plk -t ext2 -t ext3 -t ext4 -t xfs -t btrfs -t f2fs -t jfs -t reiserfs -t zfs -t vfat -t exfat -t ntfs -t ntfs3 2>/dev/null | awk 'NR>1 {p=$5; sub(/%$/, "", p); mount=$6; for(i=7;i<=NF;i++) mount=mount " " $i; print "disk=" $2 "\t" $4 "\t" p "\t" mount}'
awk 'BEGIN {n=0} {n++} END {print "processes=" n}' /proc/[0-9]*/stat 2>/dev/null
if command -v systemctl >/dev/null 2>&1; then
    units=$(systemctl --failed --no-legend --plain --no-pager 2>/dev/null) && printf '%s\n' "$units" | awk 'NF {n++} END {print "failed_units=" n+0}'
fi
printf 'collector_complete=1\n'
