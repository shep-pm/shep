#!/bin/bash
# versus-pm2.sh - head-to-head benchmark of shep against pm2.
#
# Order is A/B/A (shep, pm2, shep) so a machine state shift shows itself as
# disagreement between the two shep rounds rather than hiding in the ratio.
#
# Every metric is a function; run the whole thing or source it and call one.
#
#   versus-pm2.sh            run, then print metrics.jsonl
#   versus-pm2.sh --check    run, then judge it against baseline.json:
#                            exit 0 held, 1 regressed, 2 cannot judge
#   versus-pm2.sh --record   run, then write it to baseline.json
#
# Anything after --check is handed to compare.py, so
# `--check --threshold start_warm=20` loosens one metric for one run.
# compare.py's docstring says what each verdict means.
#
# WHY $ROOT IS NOT UNDER $SCRATCH: an AF_UNIX sun_path is 104 bytes. The
# scratch dir alone is 155 chars, so both daemons fail to bind a socket under
# it - pm2 wedges a God Daemon at 100% CPU, shep refuses outright. pm2 also
# realpath()s PM2_HOME, so a short symlink does not help. Both tools therefore
# get an equally short REAL root. Harness + raw samples still live in scratch.

set -uo pipefail

# Where builds, the pm2 install and raw samples go. Override to run elsewhere.
SCRATCH="${VERSUS_SCRATCH:-/tmp/shep-versus-pm2}"
ROOT="/private/tmp/shbvs"
RAW="$SCRATCH/versus-pm2-raw"
SHEP_BIN="${SHEP_BIN:-$SCRATCH/wt-bench/target/release/shep}"
PM2_BIN="$SCRATCH/pm2-install/node_modules/.bin/pm2"
SHEP_HOME_DIR="$ROOT/shep-bench-home"
APPS="$ROOT/apps"
export PM2_HOME="$ROOT/pm2-home"
# Next to this script, so --record writes into the checkout it runs from and
# the result is a diff to commit.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASELINE="${VERSUS_BASELINE:-$HERE/baseline.json}"

N_APPS=10
SETTLE_IDLE=10
SAMPLE_IDLE=60
SETTLE_LOG=5
SAMPLE_LOG=30
START_TIMEOUT=60
POLL_INTERVAL=0.01
LINE_BYTES=58   # verified at runtime by check_line_bytes

METRICS="$RAW/metrics.jsonl"

# ---------------------------------------------------------------- helpers --

shepc() { "$SHEP_BIN" --home "$SHEP_HOME_DIR" --style bare "$@"; }
pm2c()  { "$PM2_BIN" "$@"; }

now() { perl -MTime::HiRes -e 'printf "%.6f", Time::HiRes::time()'; }

# ps cputime "MM:SS.ss" / "H:MM:SS.ss" -> seconds
cputime_s() {
  ps -o cputime= -p "$1" 2>/dev/null \
    | tr -d ' ' \
    | awk -F: '{s=0; for(i=1;i<=NF;i++) s=s*60+$i; printf "%.2f", s}'
}
rss_kb() { ps -o rss= -p "$1" 2>/dev/null | tr -d ' '; }

emit() { echo "$1" >> "$METRICS"; }

# Sum RSS of processes parented by the daemon that are NOT managed apps.
helper_rss_kb() {
  local dpid="$1"; shift
  ps ax -o pid=,ppid=,rss= | awk -v d="$dpid" -v k="$*" '
    BEGIN { n=split(k,a," "); for(i=1;i<=n;i++) ex[a[i]]=1 }
    $2==d && !($1 in ex) { s+=$3 }
    END { printf "%d", s+0 }'
}

# ps %cpu + rss + cputime once per second, into a CSV.
sample_daemon() {
  local pid="$1" secs="$2" out="$3"
  echo "wall,pcpu,rss_kb,cputime_s" > "$out"
  local i
  for ((i=0; i<secs; i++)); do
    local line
    line=$(ps -o %cpu=,rss=,cputime= -p "$pid" 2>/dev/null \
      | awk '{t=$3; n=split(t,p,":"); s=0; for(j=1;j<=n;j++) s=s*60+p[j];
              printf "%s,%s,%.2f", $1, $2, s}')
    [ -z "$line" ] && line="NA,NA,NA"
    echo "$(now),$line" >> "$out"
    sleep 1
  done
}

# mean/max of the pcpu column
stats_csv() {
  python3 - "$1" <<'PY'
import sys, csv
rows = list(csv.DictReader(open(sys.argv[1])))
v = [float(r["pcpu"]) for r in rows if r["pcpu"] not in ("NA", "")]
print(f'{sum(v)/len(v):.3f} {max(v):.3f} {len(v)}' if v else '0 0 0')
PY
}

# ------------------------------------------------------------ config gen --

gen_flockfile() { # kind outfile
  local kind="$1" out="$2" i
  : > "$out"
  if [ "$kind" = quiet ]; then
    for ((i=0; i<N_APPS; i++)); do
      printf '[[app]]\nname = "q%d"\nscript = "%s/quiet.sh"\n\n' "$i" "$APPS" >> "$out"
    done
  else
    printf '[[app]]\nname = "loud"\nscript = "%s/loud.sh"\n' "$APPS" >> "$out"
  fi
}

# pm2 ecosystem format, per pm2's own `pm2 ecosystem simple` generator.
# interpreter is pinned to /bin/sh so both tools execute the identical script
# under the identical shell (pm2 otherwise picks /bin/bash, a different binary).
gen_ecosystem() { # kind outfile
  local kind="$1" out="$2" i
  {
    echo "module.exports = {"
    echo "  apps : ["
    if [ "$kind" = quiet ]; then
      for ((i=0; i<N_APPS; i++)); do
        printf '    { name: "q%d", script: "%s/quiet.sh", interpreter: "/bin/sh" },\n' "$i" "$APPS"
      done
    else
      printf '    { name: "loud", script: "%s/loud.sh", interpreter: "/bin/sh" }\n' "$APPS"
    fi
    echo "  ]"
    echo "}"
  } > "$out"
}

# --------------------------------------------------------------- lifecycle --

reset_shep() {
  shepc delete all  >/dev/null 2>&1
  shepc kill        >/dev/null 2>&1
  sleep 2
  rm -f "$SHEP_HOME_DIR"/logs/*.log
  # shep refuses a --home that does not exist rather than creating one, so
  # the directory has to be here before the first start. That guard landed
  # after this harness was written and made every timed start fail with
  # "no flock at", which reads in the output as a start failure rather than
  # as a missing directory.
  mkdir -p "$SHEP_HOME_DIR"
}

reset_pm2() {
  pm2c delete all >/dev/null 2>&1
  pm2c kill       >/dev/null 2>&1
  sleep 2
  rm -f "$PM2_HOME"/logs/*.log
}

shep_online() {
  shepc --format json flock 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(0); sys.exit()
print(sum(1 for a in d.get("data", []) if a.get("status") == "online"))'
}

pm2_online() {
  pm2c jlist 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(0); sys.exit()
print(sum(1 for a in d if a.get("pm2_env", {}).get("status") == "online"))'
}

shep_pids() {
  shepc --format json flock 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin)
print(" ".join(str(a["pid"]) for a in d.get("data", []) if a.get("pid")))' 2>/dev/null
}

pm2_pids() {
  pm2c jlist 2>/dev/null | python3 -c '
import sys, json
print(" ".join(str(a["pid"]) for a in json.load(sys.stdin) if a.get("pid")))' 2>/dev/null
}

shep_dpid() { cat "$SHEP_HOME_DIR/pids/shepd.pid" 2>/dev/null; }
pm2_dpid()  { cat "$PM2_HOME/pm2.pid" 2>/dev/null; }

# --------------------------------------------------- metric 1+2: idle CPU --

# The two workloads every metric runs. Written here rather than assumed on
# disk: the generated configs name these paths, and a missing script makes a
# tool report zero online forever.
write_workloads() {
  mkdir -p "$APPS"
  printf '#!/bin/sh\nwhile true; do sleep 5; done\n' > "$APPS/quiet.sh"
  # 58 bytes per line, fixed, so a byte delta divides into an exact line count.
  printf '#!/bin/sh\nwhile true; do echo "%s"; done\n' \
    "shep versus pm2 benchmark line, fixed width payload xxxxx" > "$APPS/loud.sh"
  chmod +x "$APPS/quiet.sh" "$APPS/loud.sh"
  [ -x "$APPS/quiet.sh" ] && [ -x "$APPS/loud.sh" ] || {
    echo "workload scripts missing under $APPS" >&2; return 1; }
}

m_idle() { # tool tag
  local tool="$1" tag="$2" cfg dpid n kids hrss
  echo "[idle] $tool $tag"
  if [ "$tool" = shep ]; then
    reset_shep
    cfg="$ROOT/Flockfile.quiet.toml"; gen_flockfile quiet "$cfg"
    shepc start "$cfg" >/dev/null 2>&1
    n=$(shep_online); dpid=$(shep_dpid); kids=$(shep_pids)
  else
    reset_pm2
    cfg="$ROOT/ecosystem.quiet.config.js"; gen_ecosystem quiet "$cfg"
    pm2c start "$cfg" >/dev/null 2>&1
    n=$(pm2_online); dpid=$(pm2_dpid); kids=$(pm2_pids)
  fi
  echo "  online=$n dpid=$dpid"
  sleep "$SETTLE_IDLE"

  # Both start commands return before the children are all up, so the counts
  # taken above are pre-settle. Re-read them here or a late child is counted
  # as helper RSS and the reported online count is stale.
  if [ "$tool" = shep ]; then n=$(shep_online); kids=$(shep_pids)
  else n=$(pm2_online); kids=$(pm2_pids); fi
  [ "${n:-0}" -eq "$N_APPS" ] || {
    echo "  only $n/$N_APPS online after settle, refusing to report" >&2; return 1; }

  local rss; rss=$(rss_kb "$dpid")
  hrss=$(helper_rss_kb "$dpid" $kids)

  local c0 t0 c1 t1
  c0=$(cputime_s "$dpid"); t0=$(now)
  sample_daemon "$dpid" "$SAMPLE_IDLE" "$RAW/idle-$tool-$tag.csv"
  c1=$(cputime_s "$dpid"); t1=$(now)

  local s; s=$(stats_csv "$RAW/idle-$tool-$tag.csv")
  local mean max cnt; read -r mean max cnt <<< "$s"
  local derived
  derived=$(python3 -c "print(f'{($c1-$c0)/($t1-$t0)*100:.3f}')")

  emit "{\"metric\":\"idle\",\"tool\":\"$tool\",\"tag\":\"$tag\",\"online\":$n,\
\"pcpu_mean\":$mean,\"pcpu_max\":$max,\"samples\":$cnt,\
\"cputime_derived_pct\":$derived,\"rss_kb\":${rss:-0},\"helper_rss_kb\":${hrss:-0}}"
  echo "  mean=$mean max=$max derived=$derived rss_kb=$rss helper_rss_kb=$hrss"
}

# ------------------------------------------------- metric 3: log-plane CPU --

check_line_bytes() { # file -> bytes per line, from the first 100 lines
  head -n 100 "$1" | wc -c | awk '{printf "%d", $1/100}'
}

m_log() { # tool tag
  local tool="$1" tag="$2" cfg dpid logf
  echo "[log] $tool $tag"
  if [ "$tool" = shep ]; then
    reset_shep
    cfg="$ROOT/Flockfile.loud.toml"; gen_flockfile loud "$cfg"
    shepc start "$cfg" >/dev/null 2>&1
    dpid=$(shep_dpid)
    logf=$(shepc --format json flock 2>/dev/null | python3 -c '
import sys, json; print(json.load(sys.stdin)["data"][0]["out_file"])')
  else
    reset_pm2
    cfg="$ROOT/ecosystem.loud.config.js"; gen_ecosystem loud "$cfg"
    pm2c start "$cfg" >/dev/null 2>&1
    dpid=$(pm2_dpid)
    logf=$(pm2c jlist 2>/dev/null | python3 -c '
import sys, json; print(json.load(sys.stdin)[0]["pm2_env"]["pm_out_log_path"])')
  fi
  echo "  dpid=$dpid log=$logf"
  sleep "$SETTLE_LOG"

  # Verify the file is actually growing on disk before sampling anything.
  local g0 g1 growing
  g0=$(stat -f %z "$logf" 2>/dev/null || echo 0)
  sleep 1
  g1=$(stat -f %z "$logf" 2>/dev/null || echo 0)
  growing=$(( g1 > g0 ? 1 : 0 ))
  echo "  growing=$growing ($g0 -> $g1 bytes)"

  local lb; lb=$(check_line_bytes "$logf")

  # What does the daemon write per line, beyond the app's own out log? Snapshot
  # every file in the log dir before and after; anything that grew is per-line
  # work. Observed, not assumed.
  local logdir; logdir=$(dirname "$logf")
  ls -l "$logdir" > "$RAW/logdir-$tool-$tag.before.txt" 2>&1

  # Byte deltas, not `wc -l`: stat is instantaneous, while wc -l on a
  # multi-hundred-MB file takes long enough to smear the window boundary.
  local b0 c0 t0 b1 c1 t1
  b0=$(stat -f %z "$logf"); c0=$(cputime_s "$dpid"); t0=$(now)
  sample_daemon "$dpid" "$SAMPLE_LOG" "$RAW/log-$tool-$tag.csv"
  b1=$(stat -f %z "$logf"); c1=$(cputime_s "$dpid"); t1=$(now)
  ls -l "$logdir" > "$RAW/logdir-$tool-$tag.after.txt" 2>&1

  # Record the effective per-app log settings, to show they are defaults.
  if [ "$tool" = pm2 ]; then
    pm2c jlist 2>/dev/null | python3 -c '
import sys, json
e = json.load(sys.stdin)[0]["pm2_env"]
keys = ["pm_out_log_path","pm_err_log_path","log_date_format","merge_logs",
        "log_type","out_file","error_file","disable_logs","vizion","autorestart",
        "exec_mode","instances","pmx","automation","treekill","windowsHide"]
print(json.dumps({k: e.get(k) for k in keys}, indent=1))' \
      > "$RAW/pm2-logsettings-$tag.json" 2>&1
  fi
  # First 3 lines exactly as written, to show prefix/timestamp bytes (or none).
  head -n 3 "$logf" | cat -v > "$RAW/logsample-$tool-$tag.txt" 2>&1

  local s; s=$(stats_csv "$RAW/log-$tool-$tag.csv")
  local mean max cnt; read -r mean max cnt <<< "$s"

  # A log that stopped growing, or a zero line width, would divide by zero
  # below and leave the metric record silently absent.
  [ "${lb:-0}" -gt 0 ] || { echo "  line width is 0, refusing to derive" >&2; return 1; }
  [ "$b1" -gt "$b0" ] || { echo "  log did not grow during the window" >&2; return 1; }

  local res
  res=$(python3 -c "
b0,b1,c0,c1,t0,t1,lb = $b0,$b1,$c0,$c1,$t0,$t1,$lb
el = t1-t0; lines = (b1-b0)/lb; cpu = c1-c0
print(f'{lines:.0f} {lines/el:.1f} {cpu/lines*1e6:.4f} {cpu/el*100:.3f} {el:.2f} {cpu:.2f}')")
  local lines lps uspl dpct el cpu
  read -r lines lps uspl dpct el cpu <<< "$res"

  emit "{\"metric\":\"log\",\"tool\":\"$tool\",\"tag\":\"$tag\",\"growing\":$growing,\
\"line_bytes\":$lb,\"pcpu_mean\":$mean,\"pcpu_max\":$max,\
\"cputime_derived_pct\":$dpct,\"lines\":$lines,\"lines_per_s\":$lps,\
\"us_per_line\":$uspl,\"elapsed_s\":$el,\"daemon_cpu_s\":$cpu,\"log\":\"$logf\"}"
  echo "  mean=$mean derived=$dpct lines/s=$lps us/line=$uspl"
}

# ------------------------------------------------- metric 4: start latency --

# One timed start: invoke the start command, then poll the tool's own list
# command until all N report online. Returns "seconds online".
timed_start() { # tool cfg
  local tool="$1" cfg="$2" t0 t1 n
  t0=$(now)
  # Deadline and interval both matter. Unbounded, a failed start spins the
  # list command forever and its own CPU lands in the measurement it is
  # supposed to be timing.
  local deadline; deadline=$(python3 -c "print($t0 + $START_TIMEOUT)")
  if [ "$tool" = shep ]; then
    shepc start "$cfg" >/dev/null 2>&1 || { echo "  shep start failed" >&2; return 1; }
    n=$(shep_online)
    while [ "${n:-0}" -lt "$N_APPS" ]; do
      python3 -c "import sys; sys.exit(0 if $(now) < $deadline else 1)" \
        || { echo "  timed out at $n/$N_APPS" >&2; return 1; }
      sleep "$POLL_INTERVAL"; n=$(shep_online)
    done
  else
    pm2c start "$cfg" >/dev/null 2>&1 || { echo "  pm2 start failed" >&2; return 1; }
    n=$(pm2_online)
    while [ "${n:-0}" -lt "$N_APPS" ]; do
      python3 -c "import sys; sys.exit(0 if $(now) < $deadline else 1)" \
        || { echo "  timed out at $n/$N_APPS" >&2; return 1; }
      sleep "$POLL_INTERVAL"; n=$(pm2_online)
    done
  fi
  t1=$(now)
  # Exactly the configured count. A surplus means a stale process survived a
  # previous round, and the time it took to see it is not this run's number.
  [ "${n:-0}" -eq "$N_APPS" ] || {
    echo "  saw $n online, expected exactly $N_APPS" >&2; return 1; }
  echo "$(python3 -c "print(f'{$t1-$t0:.4f}')") $n"
}

# Measured twice, because neither reading alone is the whole story:
#   cold - daemon down, so `start` also pays daemon boot. What a user feels on
#          a fresh box, and symmetric: `shep ping`/`shep set` do NOT boot shep's
#          daemon, so only `start` can, while `pm2 ping` DOES boot God. Warming
#          via ping would have handed pm2 a warm daemon and shep a cold one.
#   warm - daemon already up with zero apps. Isolates the spawn-10-apps path.
m_start() { # tool tag
  local tool="$1" tag="$2" cfg
  echo "[start] $tool $tag"
  if [ "$tool" = shep ]; then
    reset_shep   # kills the daemon
    cfg="$ROOT/Flockfile.quiet.toml"; gen_flockfile quiet "$cfg"
  else
    reset_pm2
    cfg="$ROOT/ecosystem.quiet.config.js"; gen_ecosystem quiet "$cfg"
  fi

  local cold n_cold; read -r cold n_cold <<< "$(timed_start "$tool" "$cfg")"

  # Delete the apps but leave the daemon up -> warm.
  if [ "$tool" = shep ]; then shepc delete all >/dev/null 2>&1
  else pm2c delete all >/dev/null 2>&1; fi
  sleep 2
  local warm n_warm; read -r warm n_warm <<< "$(timed_start "$tool" "$cfg")"

  # Cost of ONE list round trip, so poll overhead can be separated out:
  # pm2's list is a fresh node process, shep's is a small static binary.
  local l0 l1 listcost
  l0=$(now)
  if [ "$tool" = shep ]; then shepc --format json flock >/dev/null 2>&1
  else pm2c jlist >/dev/null 2>&1; fi
  l1=$(now)
  listcost=$(python3 -c "print(f'{$l1-$l0:.4f}')")

  emit "{\"metric\":\"start\",\"tool\":\"$tool\",\"tag\":\"$tag\",\
\"cold_s\":$cold,\"cold_online\":$n_cold,\"warm_s\":$warm,\"warm_online\":$n_warm,\
\"list_roundtrip_s\":$listcost}"
  echo "  cold=${cold}s warm=${warm}s online=$n_cold/$n_warm list_roundtrip=${listcost}s"
}

# ------------------------------------------------------ metric 5: footprint --

# Every [[bin]] the shep package declares: what `cargo install shep`, the
# .deb and the release archives all put on disk. Read from the manifest, so
# a new [[bin]] is counted too.
shep_bin_names() {
  awk '/^\[\[bin\]\]/ { inbin = 1; next }
       /^\[/         { inbin = 0 }
       inbin && /^name = / { gsub(/"/, "", $3); print $3 }' \
    "$SCRATCH/wt-bench/crates/shep-cli/Cargo.toml"
}

bytes_of() { wc -c < "$1" | tr -d ' '; }

# The install is every binary beside $SHEP_BIN. A missing one refuses the
# metric rather than reporting part of the install as all of it.
m_footprint() {
  echo "[footprint]"
  local dir names name b total=0 per="" pm gz
  dir=$(dirname "$SHEP_BIN")
  names=$(shep_bin_names)
  [ -n "$names" ] || { echo "  no [[bin]] names found in the manifest" >&2; return 1; }
  for name in $names; do
    [ -f "$dir/$name" ] || { echo "  $dir/$name is missing, refusing to report" >&2; return 1; }
    b=$(bytes_of "$dir/$name")
    total=$((total + b))
    per="$per\"$name\":$b,"
    python3 -c "print(f'  $name {$b/1048576:.2f} MiB')"
  done
  # What a release archive weighs: the same binaries, tarred and gzipped.
  # shellcheck disable=SC2086 # one word per binary name is the point
  gz=$(tar -czf - -C "$dir" $names | wc -c | tr -d ' ')
  pm=$(du -sk "$SCRATCH/pm2-install" | awk '{print $1}')
  emit "{\"metric\":\"footprint\",\"shep_binaries_bytes\":{${per%,}},\
\"shep_install_bytes\":$total,\"shep_archive_gz_bytes\":$gz,\"pm2_install_kb\":$pm}"
  python3 -c "print(f'  shep install {$total/1048576:.2f} MiB, {$gz/1048576:.2f} MiB gzipped   pm2 install {$pm/1024:.2f} MiB')"
}

m_versions() {
  local sha pv nv
  # The checkout SHEP_BIN was built in, not a fixed worktree: a SHEP_BIN
  # pointed elsewhere otherwise records another build's commit, and a
  # recorded baseline carries that commit into the repository. Empty when
  # the binary is not under a checkout, which compare.py reads as unknown.
  sha=$(git -C "$(dirname "$SHEP_BIN")" rev-parse HEAD 2>/dev/null)
  pv=$(node -e "console.log(require('$SCRATCH/pm2-install/node_modules/pm2/package.json').version)")
  nv=$(node --version)
  emit "{\"metric\":\"versions\",\"shep_sha\":\"$sha\",\"shep_version\":\"$("$SHEP_BIN" --version | head -1 | awk '{print $2}')\",\"pm2\":\"$pv\",\"node\":\"$nv\"}"
  echo "  shep $sha | pm2 $pv | node $nv"
}

# The run's date and platform, which compare.py needs before it will judge
# anything: a baseline only judges runs from its own OS and architecture.
# Built with json.dumps rather than by hand, since a CPU brand string is not
# ours to trust to be quote-free.
m_run() {
  emit "$(python3 - "$(date +%F)" "$(uname -s)" "$(uname -m)" \
    "$(sysctl -n machdep.cpu.brand_string 2>/dev/null)" \
    "$(sysctl -n hw.ncpu 2>/dev/null)" <<'PY'
import json, sys
date, os_, arch, cpu, ncpu = sys.argv[1:]
print(json.dumps({"metric": "run", "date": date, "os": os_, "arch": arch,
                  "cpu": cpu or None, "ncpu": int(ncpu) if ncpu.isdigit() else None}))
PY
)"
}

# Everything a run needs and does not make for itself, checked before
# anything is killed or started. Missing, each one surfaces minutes in as a
# start failure in the first round rather than as the thing that is missing.
preflight() {
  local ok=0 tool names name
  # `stat -f %z`, `sysctl machdep` and a /private/tmp root are all macOS.
  # GNU stat reads `-f` as "report the filesystem" and `%z` as a second
  # file to report on, so every size this takes would be a paragraph of
  # filesystem statistics rather than a number.
  [ "$(uname -s)" = Darwin ] || {
    echo "this harness is macOS-only; it reads BSD stat and sysctl" >&2; ok=1; }
  [ -x "$SHEP_BIN" ] || {
    echo "no shep binary at $SHEP_BIN: build one, or set SHEP_BIN" >&2; ok=1; }
  # m_footprint sizes every [[bin]] beside SHEP_BIN and refuses the metric
  # when one is missing, which it finds out last, after the whole run, and
  # a --record then refuses to write anything.
  names=$(shep_bin_names 2>/dev/null)
  [ -n "$names" ] || {
    echo "no [[bin]] names in $SCRATCH/wt-bench/crates/shep-cli/Cargo.toml for the footprint" >&2; ok=1; }
  for name in $names; do
    [ -f "$(dirname "$SHEP_BIN")/$name" ] || {
      echo "no $name beside $SHEP_BIN: the footprint sizes every [[bin]]" >&2; ok=1; }
  done
  [ -x "$PM2_BIN" ] || {
    echo "no pm2 at $PM2_BIN: npm install --prefix $SCRATCH/pm2-install pm2@<version>," >&2
    echo "  the baseline's version for --check, so the control is the same program" >&2
    ok=1; }
  for tool in python3 perl node; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$tool is not on PATH" >&2; ok=1; }
  done
  if [ "$MODE" = check ] && [ ! -r "$BASELINE" ]; then
    echo "no baseline at $BASELINE to check against" >&2; ok=1
  fi
  return "$ok"
}

# Idempotent: the trap and the normal path both call this.
_cleaned=0
cleanup_all() {
  [ "$_cleaned" = 1 ] && return 0
  _cleaned=1
  echo "[cleanup]"
  shepc delete all >/dev/null 2>&1
  shepc kill       >/dev/null 2>&1
  pm2c delete all  >/dev/null 2>&1
  pm2c kill        >/dev/null 2>&1
  sleep 2
  echo "  shepd pid file:"; cat "$SHEP_HOME_DIR/pids/shepd.pid" 2>/dev/null || echo "   (gone)"
  echo "  pm2 god daemons:"; ps ax -o pid=,command= | grep "[P]M2 v" || echo "   none"
  echo "  workload children:"; ps ax -o pid=,command= | grep -E "[q]uiet\.sh|[l]oud\.sh" || echo "   none"
  echo "  daemons rooted at $ROOT:"; ps ax -o pid=,command= | grep "[s]hbvs" | grep -v grep || echo "   none"
  # The loud workload leaves ~GB of log behind; reclaim it.
  du -sh "$PM2_HOME/logs" "$SHEP_HOME_DIR/logs" 2>/dev/null
  rm -f "$PM2_HOME"/logs/*.log "$SHEP_HOME_DIR"/logs/*.log
  echo "  logs reclaimed"
}

# ------------------------------------------------------------------- main --

round() { # tag
  local tag="$1"
  m_idle  shep "$tag"; m_log  shep "$tag"; m_start shep "$tag"
}

usage() {
  cat <<'USAGE'
usage: versus-pm2.sh [--check [compare.py check options] | --record]
       source versus-pm2.sh --source-only    then call one m_* function

  (none)    run shep, pm2, shep, then print metrics.jsonl
  --check   run, then judge the result against baseline.json:
            exit 0 held, 1 regressed, 2 cannot judge
  --record  run, then write the result to baseline.json

VERSUS_BASELINE names another baseline file for either mode.
USAGE
}

# Read before any trap is installed or any daemon touched: a mistyped mode
# should cost nothing, not a cleanup pass over the bench daemons. What
# follows --check is compare.py's, read when the run ends; a mistake there
# costs one re-judge of the kept samples, which main prints the command for.
MODE=run
case "${1:-}" in
  --source-only) MODE=source ;;
  --check)       MODE=check; shift ;;
  --record)      MODE=record; shift
                 [ $# -eq 0 ] || { usage >&2; exit 2; } ;;
  -h|--help)     usage; exit 0 ;;
  "")            ;;
  *)             usage >&2; exit 2 ;;
esac
if [ "$MODE" != source ]; then
  preflight || exit 2
fi
mkdir -p "$RAW" "$APPS"

# Installed here, below cleanup_all's definition: a trap set before the
# function exists fires into an unbound name on an early interrupt.
#
# INT and TERM get their own handler because EXIT alone would clean up and
# then let the next round restart everything the operator just interrupted.
# 128+signal is the shell's own convention for a signal death.
trap cleanup_all EXIT
trap 'cleanup_all; exit 130' INT
trap 'cleanup_all; exit 143' TERM

main() {
  : > "$METRICS"
  echo "=== versus-pm2 :: $(date) ==="
  # The generated configs name these two scripts by path, and a tool whose
  # script is missing reports zero online forever, which surfaces as a start
  # failure rather than as a missing file. The function existed and nothing
  # called it.
  write_workloads || exit 1
  m_versions
  m_run
  echo "--- machine ---"
  echo "$(sysctl -n machdep.cpu.brand_string) / $(sysctl -n hw.ncpu) cpus"
  pmset -g batt | head -2
  uptime

  # A / B / A
  echo; echo "########## ROUND A1 (shep) ##########"
  m_idle shep A1; m_log shep A1; m_start shep A1
  echo; echo "########## ROUND B  (pm2)  ##########"
  m_idle pm2  B;  m_log pm2  B;  m_start pm2  B
  echo; echo "########## ROUND A2 (shep) ##########"
  m_idle shep A2; m_log shep A2; m_start shep A2

  m_footprint
  cleanup_all
  echo; echo "=== metrics.jsonl ==="; cat "$METRICS"

  local rc=0
  case "$MODE" in
    check)
      echo; echo "=== against $BASELINE ==="
      python3 "$HERE/compare.py" check "$METRICS" --baseline "$BASELINE" "$@"
      rc=$?
      # The raw samples outlive the run, so a second opinion, or the same
      # one with a threshold moved, never needs another run.
      echo; echo "judge this run again without re-running it:"
      echo "  python3 $HERE/compare.py check $METRICS"
      ;;
    record)
      echo
      python3 "$HERE/compare.py" record "$METRICS" --out "$BASELINE"
      rc=$?
      ;;
  esac
  return "$rc"
}

[ "$MODE" = source ] || main "$@"
