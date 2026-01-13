#!/bin/bash
# Benchmark: WCGW (Python) vs Winx (Rust)

set -e

WINX="./target/release/winx-code-agent"
WCGW="wcgw"
ITERATIONS=5

echo "╔═══════════════════════════════════════════════════════════════════╗"
echo "║           BENCHMARK: WCGW (Python) vs Winx (Rust)                 ║"
echo "╚═══════════════════════════════════════════════════════════════════╝"
echo ""

# 1. MCP Server Startup Time
echo "═══════════════════════════════════════════════════════════════════"
echo "📊 TEST 1: MCP Server Startup Time"
echo "═══════════════════════════════════════════════════════════════════"

echo ""
echo "🐍 WCGW (Python):"
for i in $(seq 1 $ITERATIONS); do
    START=$(date +%s%3N)
    timeout 3s python -c "
import json
import sys
sys.path.insert(0, '.')
from wcgw.client.mcp_server import main
" 2>/dev/null || true
    END=$(date +%s%3N)
    echo "  Run $i: $((END - START))ms"
done

echo ""
echo "🦀 Winx (Rust):"
for i in $(seq 1 $ITERATIONS); do
    START=$(date +%s%3N)
    timeout 0.1s $WINX serve 2>/dev/null || true
    END=$(date +%s%3N)
    echo "  Run $i: $((END - START))ms"
done

# 2. Memory Usage
echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "📊 TEST 2: Memory Usage (RSS)"
echo "═══════════════════════════════════════════════════════════════════"

echo ""
echo "🐍 WCGW (Python):"
python -c "import wcgw" &
WCGW_PID=$!
sleep 0.5
WCGW_MEM=$(ps -o rss= -p $WCGW_PID 2>/dev/null || echo "0")
kill $WCGW_PID 2>/dev/null || true
echo "  Memory: $((WCGW_MEM / 1024))MB"

echo ""
echo "🦀 Winx (Rust):"
$WINX serve &
WINX_PID=$!
sleep 0.1
WINX_MEM=$(ps -o rss= -p $WINX_PID 2>/dev/null || echo "0")
kill $WINX_PID 2>/dev/null || true
echo "  Memory: $((WINX_MEM / 1024))MB"

# 3. Binary Size
echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "📊 TEST 3: Binary/Package Size"
echo "═══════════════════════════════════════════════════════════════════"

echo ""
echo "🐍 WCGW (Python):"
WCGW_SIZE=$(pip show wcgw 2>/dev/null | grep -i location | cut -d: -f2 | xargs -I{} du -sh {}/wcgw 2>/dev/null | cut -f1 || echo "N/A")
echo "  Package: $WCGW_SIZE"

echo ""
echo "🦀 Winx (Rust):"
WINX_SIZE=$(du -sh $WINX | cut -f1)
echo "  Binary: $WINX_SIZE"

# 4. Test Count
echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "📊 TEST 4: Test Suite"
echo "═══════════════════════════════════════════════════════════════════"

echo ""
echo "🦀 Winx tests:"
WINX_TESTS=$(cargo test 2>&1 | grep "test result" | tail -1)
echo "  $WINX_TESTS"

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "✨ BENCHMARK COMPLETE"
echo "═══════════════════════════════════════════════════════════════════"
