LC_ALL=C
export LC_ALL
printf 'hostname=%s\nkernel=%s\n' "$(hostname)" "$(uname -r)"
awk '{print "uptime_seconds=" $1}' /proc/uptime
awk '/^cpu / {idle=$5+$6; total=0; for(i=2;i<=9;i++) total+=$i; printf "cpu_total=%.0f\ncpu_idle=%.0f\n",total,idle} /^cpu[0-9]+ / {n++} END {print "cpu_count=" n}' /proc/stat
awk '{print "load1=" $1 "\nload5=" $2 "\nload15=" $3}' /proc/loadavg
awk '/^MemTotal:/ {print "memory_total_kb=" $2} /^MemAvailable:/ {print "memory_available_kb=" $2} /^SwapTotal:/ {print "swap_total_kb=" $2} /^SwapFree:/ {print "swap_free_kb=" $2}' /proc/meminfo
# Filter mount types ourselves: BusyBox df need not support GNU -l / -t.
while read -r device mount fstype rest; do
    case "$fstype" in
        ext2|ext3|ext4|xfs|btrfs|f2fs|jfs|reiserfs|zfs|vfat|exfat|ntfs|ntfs3|ubifs|jffs2)
            mount=$(printf '%b' "$mount")
            df -Pk "$mount" 2>/dev/null | awk 'NR>1 && NF>=6 {p=$5; sub(/%$/, "", p); mount=$6; for(i=7;i<=NF;i++) mount=mount " " $i; print "disk=" $2 "\t" $4 "\t" p "\t" mount}'
            ;;
    esac
done < /proc/mounts
# Count proc entries instead of relying on a particular ps implementation.
set -- /proc/[0-9]*
if [ -d "$1" ]; then
    printf 'processes=%s\n' "$#"
fi
if command -v systemctl >/dev/null 2>&1; then
    printf 'systemd_supported=1\n'
    units=$(systemctl --failed --no-legend --plain --no-pager 2>/dev/null) && printf '%s\n' "$units" | awk 'NF {n++} END {print "failed_units=" n+0}'
else
    printf 'systemd_supported=0\n'
fi
# Raw records are parsed locally; no extra SSH session is needed.
awk 'NR>2 {print "net=" $0}' /proc/net/dev 2>/dev/null
if command -v sensors >/dev/null 2>&1; then
    sensors -u 2>/dev/null | awk '{print "sensor=" $0}'
fi
printf 'collector_complete=1\n'
