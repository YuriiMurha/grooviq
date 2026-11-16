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
# Runs in the foreground until the app quits; a folder it made itself is removed then.
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
	home=$(mktemp -d "${tmp%/}/grooviq-qa.XXXXXX")
	owned=1
fi
cleanup() {
	if [ "$owned" = 1 ]; then
		rm -rf "$home"
	fi
}
trap cleanup EXIT INT TERM

echo "Silent QA run: $app (GROOVIQ_AUDIO=null), home $home"
GROOVIQ_AUDIO=null HOME="$home" CFFIXED_USER_HOME="$home" "$app/Contents/MacOS/$exe"
