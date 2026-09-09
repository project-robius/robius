#!/bin/bash
# Builds NativeSpeech.swift together with its regressions and runs them.
# You need macOS and the Xcode command line tools. No microphone is used.
set -euo pipefail

tests_dir=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/robius-speech-tests.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# The privacy keys must be embedded for the unbundled-launch regression, which
# checks that the bridge still refuses to touch a permission API without a bundle.
cat > "$tmp/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>org.robius.speech.tests</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>Privacy validation regression only; these tests do not access the microphone.</string>
    <key>NSSpeechRecognitionUsageDescription</key>
    <string>Privacy validation regression only; these tests do not request speech recognition.</string>
</dict>
</plist>
PLIST

xcrun --sdk macosx swiftc -swift-version 5 -O -parse-as-library -D ROBIUS_SPEECH_TESTS \
    "$tests_dir/../NativeSpeech.swift" "$tests_dir/SpeechTests.swift" \
    -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker "$tmp/Info.plist" \
    -o "$tmp/speech-tests"
"$tmp/speech-tests"
