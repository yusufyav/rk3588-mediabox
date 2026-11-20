#!/usr/bin/env bash
#
# `deploy-mediabox-v3.sh --only`'s appliance half (scripts/deploy-only-install.sh)
# and the rollback it writes, run for real on a scratch prefix with a stub
# systemctl and a stub /proc: nothing here touches a device or a service.
#
# The stub systemctl keeps each unit's state in a file; a restart starts a
# new "process" whose /proc/<pid>/exe is a copy of the file its ExecStart
# names at that moment -- the inode it would run -- or, told so, a stale
# binary, or it fails.
#
# SH picks the shell the script runs under (default sh), as the appliance
# runs it with `sh install.sh`.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${ONLY_INSTALL:-$here/scripts/deploy-only-install.sh}"
read -r -a shell <<<"${SH:-sh}"
failures=0

report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
okay()   { printf 'ok   %s\n' "$*"; }
check()  { if eval "$2"; then okay "$1"; else report "$1"; fi; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mediabox-deploy-only-XXXXXX")"
trap '[ -n "${KEEP:-}" ] || rm -rf "$work"' EXIT

stubs="$work/stubs"
mkdir -p "$stubs"
cat >"$stubs/systemctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$STATE/systemctl.log"
case "$1" in
  is-active)
    [ "$2" = --quiet ] && shift
    [ "$(cat "$STATE/$2.state" 2>/dev/null)" = active ]
    ;;
  restart)
    unit=$2
    [ -e "$STATE/$unit.fail" ] && exit 1
    pid=$(($(cat "$STATE/pidseq" 2>/dev/null || echo 100) + 1))
    echo "$pid" >"$STATE/pidseq"
    echo active >"$STATE/$unit.state"
    echo "$pid" >"$STATE/$unit.pid"
    mkdir -p "$PROC/$pid"
    if [ -e "$STATE/$unit.stale" ]; then
      cp "$STATE/$unit.stale" "$PROC/$pid/exe"
    else
      cp "$(cat "$STATE/$unit.exec")" "$PROC/$pid/exe"
    fi
    ;;
  stop)
    echo inactive >"$STATE/$2.state"
    ;;
  show)
    # show -p MainPID --value <unit>
    cat "$STATE/$5.pid" 2>/dev/null || echo 0
    ;;
  *) exit 2 ;;
esac
EOF
# `install` that writes something else, when told to: the copy on disk is
# not what was built.
cat >"$stubs/install" <<'EOF'
#!/bin/sh
if [ -e "$STATE/install.corrupt" ]; then
  for last; do :; done
  echo corrupt >"$last"
  exit 0
fi
exec /usr/bin/env -u STATE "$REAL_INSTALL" "$@"
EOF
chmod +x "$stubs/systemctl" "$stubs/install"
export REAL_INSTALL
REAL_INSTALL="$(command -v install)"

# A fresh appliance: <prefix>/bin with `old` binaries, units whose state and
# ExecStart are given, and a stage of `new` binaries with their sums.
#   scenario <name> "<bin>..." "<installed-before>..." "<unit>=<bin>:<state>..."
scenario() {
  W="$work/$1"
  export STATE="$W/state" PROC="$W/proc"
  prefix="$W/prefix"
  stage="$W/stage"
  mkdir -p "$STATE" "$PROC" "$prefix/bin" "$stage"
  for b in $2; do
    printf 'new %s\n' "$b" >"$stage/$b"
  done
  for b in $3; do
    printf 'old %s\n' "$b" >"$prefix/bin/$b"
    chmod 0755 "$prefix/bin/$b"
  done
  units=()
  for spec in $4; do
    pair=${spec%%:*}
    unit=${pair%%=*}
    echo "${spec##*:}" >"$STATE/$unit.state"
    echo "$prefix/bin/${pair#*=}" >"$STATE/$unit.exec"
    units+=("$pair")
  done
  (cd "$stage" && sha256sum $2 >SHA256SUMS)
  cp "$script" "$stage/install.sh"
}

run_install() {
  PATH="$stubs:$PATH" MEDIABOX_ONLY_PROC="$PROC" MEDIABOX_ONLY_SETTLE=0 \
    "${shell[@]}" "$stage/install.sh" "$prefix" "${units[@]}" >"$W/out" 2>"$W/err"
}
rollback_dir() { ls -d "$prefix"/rollback/only-* 2>/dev/null | head -1; }
run_rollback() {
  PATH="$stubs:$PATH" "${shell[@]}" "$(rollback_dir)/rollback.sh" >"$W/rb.out" 2>"$W/rb.err"
}
sha() { sha256sum "$1" | cut -d' ' -f1; }
restarts() { { cat "$STATE/systemctl.log" 2>/dev/null || true; } | grep -cx "restart $1" || true; }

echo "-- an update of binaries that are there, their services running"
scenario update "mediabox-tv mediaboxd-rs" "mediabox-tv mediaboxd-rs" \
  "mediabox-tv-ui.service=mediabox-tv:active mediaboxd-rs.service=mediaboxd-rs:active"
run_install
status=$?
check "the install succeeds" '[ $status = 0 ]'
check "the new binaries are in place" \
  '[ "$(cat "$prefix/bin/mediabox-tv")" = "new mediabox-tv" ] && [ "$(cat "$prefix/bin/mediaboxd-rs")" = "new mediaboxd-rs" ]'
check "no .new file is left" '! ls -A "$prefix/bin" | grep -q "^\."'
R=$(rollback_dir)
check "the rollback keeps the binaries it replaced" \
  '[ "$(cat "$R/bin/mediabox-tv")" = "old mediabox-tv" ] && [ ! -s "$R/absent" ]'
check "the rollback records each unit as it was" \
  'grep -qx "mediabox-tv-ui.service active mediabox-tv" "$R/units" && grep -qx "mediaboxd-rs.service active mediaboxd-rs" "$R/units"'
check "each running unit is restarted once" \
  '[ "$(restarts mediabox-tv-ui.service)" = 1 ] && [ "$(restarts mediaboxd-rs.service)" = 1 ]'
check "each process is checked by its full sha256" \
  'grep -q "mediabox-tv-ui.service active, pid [0-9]*, running mediabox-tv $(sha "$stage/mediabox-tv")$" "$W/out"'
check "the rollback command is said" 'grep -qx "rollback: sh $R/rollback.sh" "$W/out"'
run_rollback
status=$?
check "the rollback succeeds" '[ $status = 0 ]'
check "the rollback puts the old binaries back" \
  '[ "$(cat "$prefix/bin/mediabox-tv")" = "old mediabox-tv" ] && [ "$(cat "$prefix/bin/mediaboxd-rs")" = "old mediaboxd-rs" ]'
check "the rollback replaces by rename and leaves nothing behind" '! ls -A "$prefix/bin" | grep -q "^\."'
check "the rollback restarts the units that were running" \
  '[ "$(restarts mediabox-tv-ui.service)" = 2 ] && [ "$(restarts mediaboxd-rs.service)" = 2 ]'

echo "-- a first install of a binary that was not there"
scenario first "mediabox-dvb mediabox-display-observer" "" \
  "mediabox-display-observer.service=mediabox-display-observer:inactive"
run_install
status=$?
check "the install succeeds" '[ $status = 0 ]'
R=$(rollback_dir)
check "nothing to keep is no error, and both are noted as new" \
  '[ ! -s "$R/SHA256SUMS" ] && grep -qx mediabox-dvb "$R/absent" && grep -qx mediabox-display-observer "$R/absent"'
check "the new binaries are in place" '[ "$(cat "$prefix/bin/mediabox-dvb")" = "new mediabox-dvb" ]'
run_rollback
status=$?
check "the rollback of a first install succeeds" '[ $status = 0 ]'
check "the rollback removes what the install added" \
  '[ ! -e "$prefix/bin/mediabox-dvb" ] && [ ! -e "$prefix/bin/mediabox-display-observer" ]'
check "and stops the service the install started for it, as it was before" \
  '[ "$(cat "$STATE/mediabox-display-observer.service.state")" = inactive ] && grep -q "stopped: its binary was new" "$W/rb.out"'

echo "-- the interface not running: not started, by the install or its rollback"
scenario idle "mediabox-tv" "mediabox-tv" "mediabox-tv-ui.service=mediabox-tv:inactive"
run_install
status=$?
check "the install succeeds" '[ $status = 0 ]'
check "the interface is not started" \
  '[ "$(restarts mediabox-tv-ui.service)" = 0 ] && [ "$(cat "$STATE/mediabox-tv-ui.service.state")" = inactive ]'
check "it is said" 'grep -q "mediabox-tv-ui.service is not running; not started" "$W/out"'
check "the rollback records it stopped" 'grep -qx "mediabox-tv-ui.service inactive mediabox-tv" "$(rollback_dir)/units"'
run_rollback
status=$?
check "the rollback succeeds" '[ $status = 0 ]'
check "the rollback does not start it either" \
  '[ "$(restarts mediabox-tv-ui.service)" = 0 ] && [ "$(cat "$STATE/mediabox-tv-ui.service.state")" = inactive ]'
check "and says so" 'grep -q "mediabox-tv-ui.service left stopped" "$W/rb.out"'

echo "-- a stopped service other than the interface is not started by the rollback"
scenario stopped "mediaboxd-rs" "mediaboxd-rs" "mediaboxd-rs.service=mediaboxd-rs:inactive"
run_install
echo inactive >"$STATE/mediaboxd-rs.service.state"
run_rollback
check "a unit stopped before and after is left stopped" \
  '[ "$(cat "$STATE/mediaboxd-rs.service.state")" = inactive ] && [ "$(restarts mediaboxd-rs.service)" = 1 ]'

echo "-- a restart that fails"
scenario restart-fails "mediaboxd-rs" "mediaboxd-rs" "mediaboxd-rs.service=mediaboxd-rs:active"
touch "$STATE/mediaboxd-rs.service.fail"
run_install
status=$?
R=$(rollback_dir)
check "the install fails" '[ $status != 0 ]'
check "it says which unit, and the rollback command" \
  'grep -q "mediaboxd-rs.service did not restart" "$W/err" && grep -qx "rollback: sh $R/rollback.sh" "$W/err"'
rm "$STATE/mediaboxd-rs.service.fail"
echo failed >"$STATE/mediaboxd-rs.service.state"
run_rollback
check "the rollback restores the binary and restarts the unit that was running" \
  '[ "$(cat "$prefix/bin/mediaboxd-rs")" = "old mediaboxd-rs" ] && [ "$(cat "$STATE/mediaboxd-rs.service.state")" = active ]'

echo "-- a process that does not run what was installed"
scenario stale "mediabox-tv" "mediabox-tv" "mediabox-tv-ui.service=mediabox-tv:active"
printf 'old mediabox-tv\n' >"$STATE/mediabox-tv-ui.service.stale"
run_install
status=$?
check "the install fails" '[ $status != 0 ]'
check "it names the unit and both sums, and the rollback command" \
  'grep -q "mediabox-tv-ui.service (pid [0-9]*) runs $(printf "old mediabox-tv\n" | sha256sum | cut -d" " -f1), not the installed mediabox-tv $(sha "$stage/mediabox-tv")" "$W/err" && grep -q "^rollback: sh " "$W/err"'

echo "-- a copy on disk that is not what was built"
scenario corrupt "mediabox-tv" "mediabox-tv" "mediabox-tv-ui.service=mediabox-tv:active"
touch "$STATE/install.corrupt"
run_install
status=$?
rm "$STATE/install.corrupt"
check "the install fails" '[ $status != 0 ]'
check "before any restart" '[ "$(restarts mediabox-tv-ui.service)" = 0 ]'
check "with the rollback command" \
  'grep -q "differ from the ones built" "$W/err" && grep -q "^rollback: sh " "$W/err"'

echo "-- binaries that are not the ones built"
scenario unbuilt "mediabox-tv" "mediabox-tv" "mediabox-tv-ui.service=mediabox-tv:active"
printf 'tampered\n' >"$stage/mediabox-tv"
run_install
status=$?
check "nothing is done at all" \
  '[ $status != 0 ] && [ -z "$(rollback_dir)" ] && [ "$(cat "$prefix/bin/mediabox-tv")" = "old mediabox-tv" ] && [ "$(restarts mediabox-tv-ui.service)" = 0 ]'

echo "-- two installs in the same second keep two rollbacks"
scenario twice "mediaboxd-rs" "mediaboxd-rs" "mediaboxd-rs.service=mediaboxd-rs:active"
run_install
run_install
check "each has its own" '[ "$(ls -d "$prefix"/rollback/only-* | wc -l)" = 2 ]'

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
