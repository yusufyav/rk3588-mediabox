#!/bin/sh
#
# The appliance's half of `deploy-mediabox-v3.sh --only`: puts some of the
# product's binaries in place and restarts the services that run them.
#
#   sh install.sh <prefix> [<unit>=<binary> ...]
#
# Run from the directory the binaries were copied to, beside their
# SHA256SUMS. Each <unit>=<binary> names a service and the binary its main
# process is (its ExecStart); a binary no service runs is only installed.
#
# In order, and stopping at the first thing that is not so:
#   1. the binaries are the ones that were built (SHA256SUMS);
#   2. a rollback is written first: the binaries being replaced, which ones
#      did not exist, and whether each unit was running;
#   3. each binary goes in by rename (.<name>.new, then mv), so nothing ever
#      runs half a file;
#   4. the files on disk are checked against SHA256SUMS before anything is
#      restarted;
#   5. each unit that was running is restarted -- the interface's is not
#      started if it was not running: which process owns the display is the
#      control plane's to say -- and the process each one runs afterwards is
#      checked, by its full sha256, to be the binary just installed.
# Any failure says the rollback command to run.
#
# MEDIABOX_ONLY_PROC and MEDIABOX_ONLY_SETTLE exist for
# tests/check-deploy-only.sh: where /proc is and how long a restarted unit
# is given before it is checked. Unset, /proc and 2 seconds.
set -eu

prefix=$1
shift
here=$(cd "$(dirname "$0")" && pwd)
proc=${MEDIABOX_ONLY_PROC:-/proc}
settle=${MEDIABOX_ONLY_SETTLE:-2}
cd "$here"

# 1. What was built.
sha256sum -c --quiet SHA256SUMS
names=$(awk '{print $2}' SHA256SUMS)

# 2. The way back, before anything is touched. A second install within the
# same second gets a directory of its own, never the first one's.
mkdir -p "$prefix/rollback"
R=$prefix/rollback/only-$(date -u +%Y%m%dT%H%M%SZ)
n=0
while [ -e "$R" ]; do
  n=$((n + 1))
  R=$prefix/rollback/only-$(date -u +%Y%m%dT%H%M%SZ).$n
done
mkdir -p "$R/bin"
printf '%s\n' "$prefix" >"$R/prefix"
: >"$R/SHA256SUMS"
: >"$R/absent"
: >"$R/units"
for b in $names; do
  if [ -e "$prefix/bin/$b" ]; then
    cp -a "$prefix/bin/$b" "$R/bin/$b"
    (cd "$R" && sha256sum "bin/$b") >>"$R/SHA256SUMS"
  else
    printf '%s\n' "$b" >>"$R/absent"
  fi
done
for pair in "$@"; do
  unit=${pair%%=*}
  if systemctl is-active --quiet "$unit"; then
    printf '%s active %s\n' "$unit" "${pair#*=}" >>"$R/units"
  else
    printf '%s inactive %s\n' "$unit" "${pair#*=}" >>"$R/units"
  fi
done
cat >"$R/rollback.sh" <<'ROLLBACK'
#!/bin/sh
# Puts back what one `deploy-mediabox-v3.sh --only` install replaced: each
# binary it replaced, by rename as the install did; each binary it added,
# removed. A unit is restarted if it was running before the install or is
# running now, so it runs what was put back -- unless it was stopped before
# the install and its binary was one the install added, which is gone now:
# it is stopped again. One that was stopped before the install and is
# stopped now stays stopped: a rollback starts nothing.
set -eu
cd "$(dirname "$0")"
prefix=$(cat prefix)
if [ -s SHA256SUMS ]; then
  sha256sum -c --quiet SHA256SUMS
fi
while read -r _ path; do
  b=${path#bin/}
  install -m 0755 "bin/$b" "$prefix/bin/.$b.rollback"
  mv -f "$prefix/bin/.$b.rollback" "$prefix/bin/$b"
done <SHA256SUMS
while read -r b; do
  rm -f "$prefix/bin/$b"
done <absent
while read -r unit before bin; do
  if [ "$before" != active ] && grep -qx "$bin" absent; then
    if systemctl is-active --quiet "$unit"; then
      systemctl stop "$unit"
      echo "$unit stopped: its binary was new and is removed"
    fi
  elif [ "$before" = active ] || systemctl is-active --quiet "$unit"; then
    systemctl restart "$unit"
    echo "$unit restarted"
  else
    echo "$unit left stopped, as it was before the install"
  fi
done <units
ROLLBACK
chmod 0755 "$R/rollback.sh"

fail() {
  echo "$*" >&2
  echo "rollback: sh $R/rollback.sh" >&2
  exit 1
}

# 3. In place, by rename.
for b in $names; do
  install -m 0755 "$b" "$prefix/bin/.$b.new" || fail "$b could not be copied into $prefix/bin"
  mv -f "$prefix/bin/.$b.new" "$prefix/bin/$b" || fail "$b could not be put in place"
done

# 4. On disk as built, before anything runs it.
(cd "$prefix/bin" && sha256sum -c --quiet "$here/SHA256SUMS") ||
  fail "the installed binaries differ from the ones built"

# 5. The services, and what they run.
restarted=""
for pair in "$@"; do
  unit=${pair%%=*}
  if [ "$unit" = mediabox-tv-ui.service ] && ! systemctl is-active --quiet "$unit"; then
    echo "$unit is not running; not started"
    continue
  fi
  systemctl restart "$unit" || fail "$unit did not restart"
  restarted="$restarted $pair"
done
[ -z "$restarted" ] || sleep "$settle"
for pair in $restarted; do
  unit=${pair%%=*}
  bin=${pair#*=}
  systemctl is-active --quiet "$unit" || fail "$unit is not active"
  pid=$(systemctl show -p MainPID --value "$unit")
  [ -n "$pid" ] && [ "$pid" != 0 ] || fail "$unit has no main process"
  want=$(awk -v b="$bin" '$2 == b {print $1}' "$here/SHA256SUMS")
  have=$(sha256sum "$proc/$pid/exe" | cut -d' ' -f1) || fail "$unit's process $pid cannot be read"
  [ -n "$want" ] && [ "$have" = "$want" ] ||
    fail "$unit (pid $pid) runs $have, not the installed $bin $want"
  echo "$unit active, pid $pid, running $bin $have"
done
echo "rollback: sh $R/rollback.sh"
