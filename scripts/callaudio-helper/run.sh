#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run ShellProbe as the shell uid on an adb device: run.sh JAR ARGS...   (ANDROID_SERIAL / adb env as usual)
set -uo pipefail
JAR="${1:?usage: run.sh shellprobe.jar capture|inject SOURCE [SECONDS] [HZ]}"; shift
adb push -q "$JAR" /data/local/tmp/gb-shellprobe.jar
adb shell "CLASSPATH=/data/local/tmp/gb-shellprobe.jar app_process /system/bin ShellProbe $*" 2>&1 | tr -d '\r' | grep -E "^(RESULT|INFO)|Exception|Error|Caused" | head -8
