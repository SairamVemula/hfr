#!/usr/bin/env bash
# End-to-end test of the projects/jobs/files API against a real Linux agent binary (fake `npx`, no Hyperframes needed).
# Usage: cargo build -p hfr-agent --release --target x86_64-unknown-linux-musl && bash scripts/e2e-agent.sh  (set HFR_AGENT_BIN to test another binary)
set -u
PASS=0; FAIL=0
ok() { PASS=$((PASS+1)); echo "  PASS  $1"; }
no() { FAIL=$((FAIL+1)); echo "  FAIL  $1  ($2)"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else no "$1" "expected '$3', got '$2'"; fi; }
jget() { python3 -c 'import sys,json; d=json.load(sys.stdin); print(eval(sys.argv[1]))' "$1"; }

BIN=${HFR_AGENT_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/x86_64-unknown-linux-musl/release/hfr-agent}
W=/tmp/hfr-api; rm -rf $W; mkdir -p $W/shim $W/data
cp "$BIN" $W/hfr-agent && chmod +x $W/hfr-agent
cat > $W/shim/npx <<'SHIM'
#!/bin/bash
echo "[shim] npx $*"
if [ -f slow ]; then sleep 300 & sleep 300; fi
echo "@hf-progress {\"code\":\"capture\",\"pct\":100}"
mkdir -p renders
head -c 200000 /dev/urandom > "renders/fake_$(date +%s%N).mp4"
echo done
SHIM
chmod +x $W/shim/npx

start_agent() {
  env "PATH=$W/shim:/usr/local/bin:/usr/bin:/bin" HFR_TOKEN=t HFR_MAX_JOBS=${MAXJOBS:-2} HFR_ROOT=$W/data HFR_ADDR=127.0.0.1:7897 $W/hfr-agent >$W/agent.log 2>&1 &
  APID=$!; sleep 1.2
}
stop_agent() { kill $APID 2>/dev/null; wait $APID 2>/dev/null; }
H="Authorization: Bearer t"; U=http://127.0.0.1:7897
api() { curl -s -H "$H" "$@"; }
code() { curl -s -o /dev/null -w '%{http_code}' -H "$H" "$@"; }
J='content-type: application/json'

echo "== setup"; start_agent
check "version api" "$(api $U/version | jget 'd["api"]')" 3

echo "== project + delta sync"
mkdir -p $W/src/assets && echo '<html>one</html>' > $W/src/index.html && echo 'alpha' > $W/src/assets/a.txt && echo 'beta' > $W/src/b.txt
PID=$(api -H "$J" -d '{"name":"demo"}' $U/projects | jget 'd["id"]')
[ -n "$PID" ] && ok "project created" || no "project created" "$PID"
check "empty project cannot render" "$(code -H "$J" -d '{"command":["npx","hyperframes","render"]}' $U/projects/$PID/jobs)" 409
manifest() { (cd $1 && find . -type f | sed 's|^\./||' | sort | while read -r f; do printf '{"path":"%s","size":%s,"sha256":"%s"}\n' "$f" "$(stat -c %s "$f")" "$(sha256sum "$f" | cut -d' ' -f1)"; done) | python3 -c 'import sys,json; print(json.dumps([json.loads(l) for l in sys.stdin]))'; }
plan() { api -H "$J" -d "$(manifest $W/src)" $U/projects/$PID/sync/plan; }
P1=$(plan); check "first plan uploads all 3 files" "$(echo "$P1" | jget 'len(d["upload"])')" 3
tar -cf $W/up1.tar -C $W/src index.html assets/a.txt b.txt
check "sync upload" "$(code -X PUT --data-binary @$W/up1.tar $U/projects/$PID/sync)" 204
check "second plan is empty (nothing to send)" "$(plan | jget 'len(d["upload"])+len(d["delete"])')" 0
echo '<html>two</html>' > $W/src/index.html; rm $W/src/b.txt
P3=$(plan); check "edited file is the only upload" "$(echo "$P3" | jget 'd["upload"]')" "['index.html']"
check "removed file is scheduled for delete" "$(echo "$P3" | jget 'd["delete"]')" "['b.txt']"
tar -cf $W/up2.tar -C $W/src index.html
check "delta upload" "$(code -X PUT --data-binary @$W/up2.tar $U/projects/$PID/sync)" 204
check "delete synced file" "$(code -H "$J" -d '{"paths":["b.txt"]}' $U/projects/$PID/sync/delete)" 204
check "plan after sync+delete is empty" "$(plan | jget 'len(d["upload"])+len(d["delete"])')" 0
check "b.txt is gone from the VPS" "$([ -e $W/data/$PID/project/b.txt ] && echo present || echo gone)" gone
check "path traversal in delete rejected" "$(code -H "$J" -d '{"paths":["../../etc/passwd"]}' $U/projects/$PID/sync/delete)" 400

echo "== two renders with different options in one project"
check "bad command rejected" "$(code -H "$J" -d '{"command":["bash","-c","id"]}' $U/projects/$PID/jobs)" 400
J1=$(api -H "$J" -d '{"command":["npx","hyperframes","render","--fps","60"]}' $U/projects/$PID/jobs | jget 'd["id"]')
sleep 2.5
check "job 1 done" "$(api $U/projects/$PID/jobs/$J1 | jget 'd["state"]')" done
check "job 1 recorded one output file" "$(api $U/projects/$PID/jobs/$J1/files | jget 'len(d)')" 1
J2=$(api -H "$J" -d '{"command":["npx","hyperframes","render","-q","delivery"]}' $U/projects/$PID/jobs | jget 'd["id"]')
sleep 2.5
check "job 2 done" "$(api $U/projects/$PID/jobs/$J2 | jget 'd["state"]')" done
check "job 2 has only ITS file (not job 1's)" "$(api $U/projects/$PID/jobs/$J2/files | jget 'len(d)')" 1
F1=$(api $U/projects/$PID/jobs/$J1/files | jget 'd[0]["path"]'); F2=$(api $U/projects/$PID/jobs/$J2/files | jget 'd[0]["path"]')
[ "$F1" != "$F2" ] && ok "the two jobs have different files" || no "different files" "$F1 vs $F2"
sleep 1.5
check "sha256 computed in the background" "$(api $U/projects/$PID/jobs/$J1/files | jget 'len(d[0]["sha256"] or "")')" 64
check "overview lists project with 2 jobs" "$(api $U/overview | jget 'len(d[0]["jobs"])')" 2
check "files are not mixed into the sync manifest" "$(plan | jget 'len(d["delete"])')" 0

echo "== download (full, Range, resume math)"
ENC1=$(python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$F1")
SZ=$(stat -c %s $W/data/$PID/project/$F1)
check "full download size" "$(api "$U/projects/$PID/jobs/$J1/file?path=$ENC1" | wc -c)" "$SZ"
check "Range returns 206" "$(code -H 'Range: bytes=100-199' "$U/projects/$PID/jobs/$J1/file?path=$ENC1")" 206
check "Range returns exactly 100 bytes" "$(api -H 'Range: bytes=100-199' "$U/projects/$PID/jobs/$J1/file?path=$ENC1" | wc -c)" 100
# resume: first 1000 bytes + the rest from byte 1000 must equal the file
api -H 'Range: bytes=0-999' "$U/projects/$PID/jobs/$J1/file?path=$ENC1" > $W/part
api -H 'Range: bytes=1000-' "$U/projects/$PID/jobs/$J1/file?path=$ENC1" >> $W/part
check "resumed download is byte-identical" "$(sha256sum < $W/part | cut -d' ' -f1)" "$(sha256sum < $W/data/$PID/project/$F1 | cut -d' ' -f1)"
check "Range past the end -> 416" "$(code -H "Range: bytes=$SZ-" "$U/projects/$PID/jobs/$J1/file?path=$ENC1")" 416
check "unknown file -> 404" "$(code "$U/projects/$PID/jobs/$J1/file?path=index.html")" 404
check "traversal -> 404" "$(code "$U/projects/$PID/jobs/$J1/file?path=../../x")" 404
check "other job's file is not reachable through this job" "$(code "$U/projects/$PID/jobs/$J2/file?path=$ENC1")" 404

echo "== deleting files, jobs, projects individually"
check "delete one file" "$(code -X DELETE "$U/projects/$PID/jobs/$J1/file?path=$ENC1")" 204
check "file gone from disk" "$([ -e $W/data/$PID/project/$F1 ] && echo present || echo gone)" gone
check "job 1 now lists no files" "$(api $U/projects/$PID/jobs/$J1/files | jget 'len(d)')" 0
check "job 2's file untouched" "$([ -e $W/data/$PID/project/$F2 ] && echo present || echo gone)" present
check "delete job 2 removes its file too" "$(code -X DELETE $U/projects/$PID/jobs/$J2)" 204
check "job 2's file removed from disk" "$([ -e $W/data/$PID/project/$F2 ] && echo present || echo gone)" gone
check "project still has its source files" "$([ -e $W/data/$PID/project/index.html ] && echo present || echo gone)" present

echo "== cancel + one-running-job rule + delete guard"
touch $W/data/$PID/project/slow
J3=$(api -H "$J" -d '{"command":["npx","hyperframes","render"]}' $U/projects/$PID/jobs | jget 'd["id"]'); sleep 1
J4=$(api -H "$J" -d '{"command":["npx","hyperframes","render","--fps","24"]}' $U/projects/$PID/jobs | jget 'd["id"]')
check "a second render in the same project waits in the queue" "$(api $U/projects/$PID/jobs/$J4 | jget 'd["state"]')" queued
check "it knows its place in line" "$(api $U/projects/$PID/jobs/$J4 | jget 'd["queue_position"]')" 1
check "status counts running and queued" "$(api $U/status | jget '[d["running_jobs"],d["queued_jobs"],d["max_concurrent"]]')" "[1, 1, 2]"
check "cannot delete a running job" "$(code -X DELETE $U/projects/$PID/jobs/$J3)" 409
check "cannot delete a project with a running job" "$(code -X DELETE $U/projects/$PID)" 409
check "cannot sync while running" "$(code -X PUT --data-binary @$W/up2.tar $U/projects/$PID/sync)" 409
check "cancel a queued render (it never runs)" "$(code -X POST $U/projects/$PID/jobs/$J4/cancel)" 202
check "queued render is cancelled" "$(api $U/projects/$PID/jobs/$J4 | jget 'd["state"]')" cancelled
check "cancel" "$(code -X POST $U/projects/$PID/jobs/$J3/cancel)" 202
sleep 1.5
check "job 3 cancelled" "$(api $U/projects/$PID/jobs/$J3 | jget 'd["state"]')" cancelled
check "no leftover children after cancel" "$(pgrep -f 'sleep 300' | wc -l)" 0
rm -f $W/data/$PID/project/slow

echo "== restart: everything comes back"
stop_agent; start_agent
check "project restored" "$(api $U/projects | jget 'len(d)')" 1
check "jobs restored" "$(api $U/projects/$PID/jobs | jget 'len(d)')" 3
check "cancelled state kept" "$(api $U/projects/$PID/jobs/$J3 | jget 'd["state"]')" cancelled
check "manifest kept (plan still empty)" "$(plan | jget 'len(d["upload"])+len(d["delete"])')" 0
echo "agent log: $(head -1 $W/agent.log)"

echo "== legacy folder from the first agent version is migrated"
stop_agent
L=$W/data/11111111-2222-3333-4444-555555555555; mkdir -p $L/project/renders
echo '<html>old</html>' > $L/project/index.html; head -c 5000 /dev/urandom > $L/project/renders/old.mp4; echo "old log" > $L/log.txt
printf '{"job":{"id":"11111111-2222-3333-4444-555555555555","name":"edit","state":"done","command":["npx","hyperframes","render"],"created_unix":100,"size_bytes":1},"started_unix":0}' > $L/meta.json
start_agent
LP=11111111-2222-3333-4444-555555555555
check "legacy project appears named 'edit'" "$(api $U/projects/$LP | jget 'd["name"]')" edit
check "legacy job kept with its output file" "$(api $U/projects/$LP/jobs/$LP/files | jget 'd[0]["path"]')" renders/old.mp4
check "legacy log kept" "$(api $U/projects/$LP/jobs/$LP/log)" "old log"
M=$(api $U/projects/$LP/manifest)
check "legacy manifest holds the source file only" "$(echo "$M" | jget '[x["path"] for x in d]')" "['index.html']"
check "delete legacy project" "$(code -X DELETE $U/projects/$LP)" 204
echo "== queue: a limit, first come first served, one render per project"
newproj() { P=$(api -H "$J" -d "{\"name\":\"$1\"}" $U/projects | jget 'd["id"]'); code -X PUT --data-binary @$W/up1.tar $U/projects/$P/sync >/dev/null; touch $W/data/$P/project/slow; echo $P; }
render() { api -H "$J" -d '{"command":["npx","hyperframes","render"]}' $U/projects/$1/jobs | jget 'd["id"]'; }
state() { api $U/projects/$1/jobs/$2 | jget 'd["state"]'; }
check "limit is 2 (HFR_MAX_JOBS)" "$(api $U/settings | jget 'd["effective"]')" 2
PA=$(newproj qa); PB=$(newproj qb); PC=$(newproj qc)
JA=$(render $PA); JB=$(render $PB); JC=$(render $PC); JA2=$(render $PA); sleep 1
check "project A renders" "$(state $PA $JA)" running
check "project B renders at the same time" "$(state $PB $JB)" running
check "project C waits: the limit of 2 is reached" "$(state $PC $JC)" queued
check "C is first in line" "$(api $U/projects/$PC/jobs/$JC | jget 'd["queue_position"]')" 1
check "a second render of A is behind C" "$(api $U/projects/$PA/jobs/$JA2 | jget 'd["queue_position"]')" 2
check "overview carries queue positions" "$(api $U/overview | jget 'sorted(j["queue_position"] for p in d for j in p["jobs"] if j["state"]=="queued")')" "[1, 2]"
check "cancel A's running render" "$(code -X POST $U/projects/$PA/jobs/$JA/cancel)" 202
sleep 1.5
check "the next in line (C) takes the free slot" "$(state $PC $JC)" running
check "A's second render still waits (limit reached)" "$(state $PA $JA2)" queued
check "raising the limit starts it" "$(code -X PUT -H "$J" -d '{"max_concurrent":3}' $U/settings)" 200
sleep 1.5
check "A's second render now runs" "$(state $PA $JA2)" running
check "limit 0 is refused" "$(code -X PUT -H "$J" -d '{"max_concurrent":0}' $U/settings)" 400
check "automatic limit again" "$(api -X PUT -H "$J" -d '{"max_concurrent":null}' $U/settings | jget 'd["max_concurrent"]')" None
check "settings survive a restart" "$(code -X PUT -H "$J" -d '{"max_concurrent":1}' $U/settings)" 200
JQ=$(render $PB); sleep 1
check "B's second render queues behind its own running one" "$(state $PB $JQ)" queued
stop_agent; pkill -f 'sleep 300' 2>/dev/null; MAXJOBS=9 start_agent
check "limit 1 remembered (a saved choice beats HFR_MAX_JOBS)" "$(api $U/settings | jget 'd["effective"]')" 1
check "a render that was running when the agent stopped is failed" "$(state $PA $JA2)" failed
sleep 1.5
check "the waiting render starts after the restart" "$(state $PB $JQ)" running
for X in "$PA" "$PB" "$PC"; do rm -f $W/data/$X/project/slow; done
for pair in "$PB $JQ" "$PC $JC"; do set -- $pair; code -X POST $U/projects/$1/jobs/$2/cancel >/dev/null; done
sleep 1.5; pkill -f 'sleep 300' 2>/dev/null
for X in "$PA" "$PB" "$PC"; do code -X DELETE $U/projects/$X >/dev/null; done
check "delete the demo project" "$(code -X DELETE $U/projects/$PID)" 204
check "nothing left" "$(api $U/projects | jget 'len(d)')" 0
check "status endpoint" "$(api $U/status | jget 'd["running_jobs"]')" 0
check "no token -> 401" "$(curl -s -o /dev/null -w '%{http_code}' $U/projects)" 401
stop_agent; pkill -f 'sleep 300' 2>/dev/null; rm -rf $W
echo; echo "RESULT: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
