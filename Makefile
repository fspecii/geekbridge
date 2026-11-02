# SPDX-License-Identifier: GPL-3.0-or-later
# Contributor entry points. Everything here is a thin wrapper over the commands in CONTRIBUTING.md; run `make help`.
# Needs GNU make and bash (Linux, macOS, WSL). First time on a machine: `make setup`.
SHELL := bash
.DEFAULT_GOAL := help
# Disk hygiene knobs for `make clean-dev`: APPLY=1 deletes (default dry run), HOURS=<idle hours>, WHAT=target|avd|cache|tmp|all,
# KEEP_AVD="name ...". See docs/disk-hygiene.md.
.PHONY: disk clean-dev help setup check fmt fmt-check clippy test android android-test desktop build perf docs-links hygiene coverage golden golden-desktop golden-android golden-update

help: ## list the targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | column -t -s $$'\t'

disk: ## report what is using disk (target/, AVDs, caches, /tmp) and what is safe to delete; deletes nothing
	scripts/disk-report.sh

clean-dev: ## reclaim idle scratch: dry run; APPLY=1 deletes; HOURS=12 WHAT=all KEEP_AVD="a b"
	scripts/clean-dev.sh $(if $(APPLY),--apply) $(if $(HOURS),--older-than $(HOURS)) $(if $(WHAT),--what $(WHAT)) $(foreach a,$(KEEP_AVD),--keep-avd $(a))

setup: ## check the toolchain and print what is missing (scripts/dev-setup.sh --install installs it)
	scripts/dev-setup.sh

check: fmt-check clippy test ## the Rust gate CI runs: fmt, clippy -D warnings, all tests

fmt: ## format all Rust code
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked

build: ## debug-build the whole Rust workspace
	cargo build --workspace --locked

# With the project-local toolchain (scripts/bootstrap-android.sh) this builds the native libraries, runs the Android
# unit tests and lint and produces dist/android/geekbridge-debug.apk. Without it, it runs the JVM unit tests and
# lint through the Gradle wrapper with your own JDK 17 and ANDROID_HOME (no native library needed for those).
android: ## Android: unit tests + lint (+ debug APK with the bootstrap toolchain)
	@if [ -d .tools/jdk ] && [ -d .tools/android-sdk ] && [ -d .tools/cargo ]; then \
	  scripts/build-android.sh; \
	else \
	  $(MAKE) android-test && echo "note: no project-local toolchain, so no APK was built. Run scripts/bootstrap-android.sh (Linux x86_64) for that."; \
	fi

android-test: ## Android JVM unit tests + lint via ./gradlew (needs JDK 17 and an Android SDK)
	cd android && ./gradlew --console=plain --no-daemon :app:testDebugUnitTest :app:lintDebug

desktop: ## build the desktop apps and start the service + home window (scripts/run-desktop.sh)
	cargo build --release --locked -p bridge-apps -p bridge-service -p bridge-kde -p bridge-calls -p bridge-update
	scripts/run-desktop.sh

perf: ## idle-service performance budget (startup, RSS, threads, wakeups, binary size)
	scripts/perf-check.sh --build

docs-links: ## check relative links in the docs
	scripts/check-doc-links.sh

coverage: ## test coverage report: Rust (cargo-llvm-cov) and Android JVM tests (JaCoCo); targets, not gates (docs/coverage.md)
	scripts/coverage.sh

hygiene: ## SPDX headers and third-party notices are current
	scripts/add-spdx.sh --check
	scripts/gen-notices.sh --check

# Visual regression (golden images, tests/golden/). Desktop needs Xvfb, xdotool, ImageMagick and python3 with Pillow;
# Android needs the project toolchain and a running emulator (scripts/golden-android.sh).
golden: golden-desktop golden-android ## compare every screen with its golden image (diffs: dist/golden-diff/)

golden-desktop:
	scripts/golden-desktop.sh --check

golden-android:
	scripts/golden-android.sh --check

golden-update: ## re-render the goldens. ONLY after a visual review: see CONTRIBUTING.md "Golden images"
	scripts/golden-desktop.sh --update
	scripts/golden-android.sh --update
