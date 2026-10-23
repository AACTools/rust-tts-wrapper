#!/usr/bin/env bash
# Local integration test: runs the factory reachability test under
# every feature combination that ships. Catches wiring gaps (#56, #62)
# without waiting for CI.
#
# Usage: ./scripts/test-integration.sh
# Env:   QWEN3_TTS_LIB=... OMNIVOICE_LIB=...  (for the offline engines)

set -euo pipefail
cd "$(dirname "$0")/.."

PASS=0
FAIL=0
SKIP=0

run_feature_test() {
    local label="$1"
    local features="$2"
    local env_prefix="${3:-}"

    echo -n "  $label: "
    if [ -n "$env_prefix" ] && [ -z "$(eval echo \$$env_prefix)" ]; then
        echo "SKIP (needs $env_prefix)"
        SKIP=$((SKIP + 1))
        return
    fi

    if cargo test --no-default-features --features "$features" --test factory_reachability 2>&1 | grep -q "test result: ok"; then
        echo "PASS"
        PASS=$((PASS + 1))
    else
        echo "FAIL"
        FAIL=$((FAIL + 1))
        cargo test --no-default-features --features "$features" --test factory_reachability 2>&1 | grep -E "FAILED|panicked|error" | head -3
    fi
}

echo "=== Factory reachability matrix ==="

# The shipped feature combinations (mirrors the publish matrix)
run_feature_test "default (system+cloud+sherpaonnx)" "system,cloud,sherpaonnx,display_names"
run_feature_test "cloud only" "cloud"
run_feature_test "system only" "system"
run_feature_test "floravox" "floravox"
run_feature_test "floravox-lexicons" "floravox-lexicons"
run_feature_test "pocket-timing" "pocket-timing"
run_feature_test "qwen3-local" "qwen3-local" "QWEN3_TTS_LIB"
run_feature_test "omnivoice-local" "omnivoice-local" "OMNIVOICE_LIB"
run_feature_test "qwen3 + omnivoice (both offline)" "qwen3-local,omnivoice-local,ssml-expand" "QWEN3_TTS_LIB"
run_feature_test "all offline together" "qwen3-local,omnivoice-local,pocket-timing,floravox" "QWEN3_TTS_LIB"

# Full default build (should always work)
echo -n "  cargo build (default): "
if cargo build 2>&1 | tail -1 | grep -q "Finished"; then
    echo "PASS"; PASS=$((PASS + 1))
else
    echo "FAIL"; FAIL=$((FAIL + 1))
fi

echo ""
echo "=== Results: $PASS pass, $FAIL fail, $SKIP skip ==="
[ "$FAIL" -eq 0 ] || exit 1
