#!/bin/sh
# Run a command next to a scripted mock model (mockllm.py) on 127.0.0.1:<port>.
#
#   with-mockllm.sh <port> <script.json> <log.jsonl> <command> [args...]
#
# Waits until the mock listens, runs the command with this script's stdin/stdout, stops the mock
# and exits with the command's status. The package harness runs it inside an empty network
# namespace, so the mock is the only reachable server.
set -u
here=$(cd "$(dirname "$0")" && pwd)
port=$1 script=$2 log=$3
shift 3
ready="$log.ready"
: >"$ready"
python3 "$here/mockllm.py" "$port" "$script" "$log" >"$ready" 2>&1 </dev/null &
mock=$!
tries=0
until grep -q '^listening' "$ready"; do
	tries=$((tries + 1))
	if [ "$tries" -gt 200 ] || ! kill -0 "$mock" 2>/dev/null; then
		echo "with-mockllm: mock model did not start:" >&2
		cat "$ready" >&2
		kill "$mock" 2>/dev/null
		exit 97
	fi
	sleep 0.05
done
"$@"
status=$?
kill "$mock" 2>/dev/null
wait "$mock" 2>/dev/null
exit "$status"
