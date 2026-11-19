#!/bin/sh
# Runs the built Grooviq.app for QA without sound and without the owner's data.
#
# GROOVIQ_AUDIO=null: the engine opens no audio device; it runs in real time and its sound is
# discarded. HOME and CFFIXED_USER_HOME point at a throwaway folder, so settings, autosaves,
# recovery files, backups and WebKit data land there, never in the owner's Library.
#
#   QA_HOME=<dir>  reuse this folder (kept afterwards), e.g. to relaunch after a Force Quit.
#   APP=<path>     the .app to run (default: the release build).
#
# Runs in the foreground until the app quits; a folder it made itself is removed then. INT or
# TERM to this script stops the app first.
set -eu

app=${APP:-target/release/bundle/macos/Grooviq.app}
if [ ! -d "$app" ]; then
	echo "No $app. Run make build-app first." >&2
	exit 1
fi
exe=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")

if [ -n "${QA_HOME:-}" ]; then
	home=$QA_HOME
	mkdir -p "$home"
	owned=0
else
	tmp=${TMPDIR:-/tmp}
	qa_template=grooviq-qa.XXXXXX
	# The Unix socket path includes this home, so automation needs a short basename.
	if [ "${QA_AUTOMATION:-0}" = 1 ]; then qa_template=gq.XXXXXX; fi
	home=$(mktemp -d "${tmp%/}/$qa_template")
	owned=1
fi
if [ "${QA_AUTOMATION:-0}" = 1 ]; then
	home=$(cd "$home" && pwd -P)
	export GROOVIQ_QA_DRIVER=1
fi
cleanup() {
	if [ "$owned" = 1 ]; then
		rm -rf "$home"
	fi
}
trap cleanup EXIT

echo "Silent QA run: $app (GROOVIQ_AUDIO=null), home $home"
GROOVIQ_AUDIO=null HOME="$home" CFFIXED_USER_HOME="$home" "$app/Contents/MacOS/$exe" &
child=$!
# Ctrl+C or a kill of this script ends the app too (a background child ignores SIGINT), and
# the folder is cleaned only once the app is gone.
trap 'kill -TERM "$child" 2>/dev/null' INT TERM
# A trapped signal interrupts `wait` before the app has ended: wait again until it has, and
# keep the status of the wait that saw it end.
status=0
wait "$child" || status=$?
while kill -0 "$child" 2>/dev/null; do
	if wait "$child"; then status=0; else status=$?; fi
done
exit "$status"
