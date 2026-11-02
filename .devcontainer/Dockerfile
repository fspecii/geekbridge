# SPDX-License-Identifier: GPL-3.0-or-later
# Linux build/test environment for GeekBridge: Rust workspace (desktop apps need the graphics/audio dev libraries),
# JDK 17 and the Android SDK for the Android JVM unit tests and lint. The NDK and the Rust Android targets are NOT
# included (they are only needed to rebuild the phone's native library; see scripts/bootstrap-android.sh).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config git curl ca-certificates unzip sudo \
      libxkbcommon-dev libwayland-dev libgl1-mesa-dev libxrandr-dev libxi-dev libpulse-dev dbus \
      openjdk-17-jdk-headless \
    && rm -rf /var/lib/apt/lists/*

# Android command-line tools (Google publishes only a SHA-1; same pin as scripts/bootstrap-android.sh), then the
# platform and build-tools the app compiles against.
ENV ANDROID_HOME=/opt/android-sdk ANDROID_SDK_ROOT=/opt/android-sdk JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64
RUN set -eux; \
    curl -fsSL -o /tmp/cmdline-tools.zip https://dl.google.com/android/repository/commandlinetools-linux-16111833_latest.zip; \
    echo "e025545c62a8e64c7559119566a569fb1dec5f60  /tmp/cmdline-tools.zip" | sha1sum -c -; \
    mkdir -p "$ANDROID_HOME/cmdline-tools"; \
    unzip -q /tmp/cmdline-tools.zip -d "$ANDROID_HOME/cmdline-tools"; \
    mv "$ANDROID_HOME/cmdline-tools/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"; \
    rm /tmp/cmdline-tools.zip; \
    yes | "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --licenses >/dev/null; \
    "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" "platforms;android-36" "build-tools;36.0.0"; \
    chown -R ubuntu:ubuntu "$ANDROID_HOME"

# The base image's `ubuntu` user (uid 1000) is the dev user; Rust lives in its home.
USER ubuntu
ENV RUSTUP_HOME=/home/ubuntu/.rustup CARGO_HOME=/home/ubuntu/.cargo PATH=/home/ubuntu/.cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && rustup component add rustfmt clippy
WORKDIR /workspaces/geekbridge
