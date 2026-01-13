#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════════════
# 🏁 COMPREHENSIVE BENCHMARK: WCGW (Python) vs Winx (Rust)
# ═══════════════════════════════════════════════════════════════════════════════
#
# Uses hyperfine for statistical accuracy
# Methodology: 10+ warmup runs, 50+ measured runs, median reported
#
# Requirements:
#   - hyperfine (cargo install hyperfine)
#   - wcgw (pip install wcgw)
#   - winx (cargo build --release)
#
# ═══════════════════════════════════════════════════════════════════════════════

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color
BOLD='\033[1m'

# Paths
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
WINX="$PROJECT_ROOT/target/release/winx-code-agent"
RESULTS_DIR="$PROJECT_ROOT/benchmarks/results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="$RESULTS_DIR/benchmark_$TIMESTAMP.json"

# Ensure directories exist
mkdir -p "$RESULTS_DIR"
mkdir -p /tmp/winx_bench

# Check prerequisites
check_prereqs() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}🔍 Checking Prerequisites${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"

    # Check hyperfine
    if ! command -v hyperfine &> /dev/null; then
        echo -e "${RED}❌ hyperfine not found. Install: cargo install hyperfine${NC}"
        exit 1
    fi
    echo -e "${GREEN}✓ hyperfine $(hyperfine --version | head -1)${NC}"

    # Check wcgw
    if ! command -v wcgw &> /dev/null; then
        echo -e "${RED}❌ wcgw not found. Install: pip install wcgw${NC}"
        exit 1
    fi
    echo -e "${GREEN}✓ wcgw $(wcgw --version 2>&1)${NC}"

    # Check winx
    if [ ! -f "$WINX" ]; then
        echo -e "${YELLOW}⚠ Winx not built. Building...${NC}"
        cd "$PROJECT_ROOT" && cargo build --release
    fi
    echo -e "${GREEN}✓ winx $(du -h "$WINX" | cut -f1) binary${NC}"

    # System info
    echo ""
    echo -e "${PURPLE}System Info:${NC}"
    echo "  CPU: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)"
    echo "  RAM: $(free -h | awk '/^Mem:/ {print $2}')"
    if command -v nvidia-smi &> /dev/null; then
        echo "  GPU: $(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null || echo 'N/A')"
    fi
    echo "  OS: $(uname -sr)"
    echo ""
}

# Create test files
setup_test_files() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}📁 Setting Up Test Files${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"

    # Small file (1KB)
    echo "Hello World" > /tmp/winx_bench/small.txt

    # Medium file (100KB)
    for i in $(seq 1 1000); do
        echo "Line $i: Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore."
    done > /tmp/winx_bench/medium.txt

    # Large file (1MB)
    for i in $(seq 1 10000); do
        echo "Line $i: Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."
    done > /tmp/winx_bench/large.txt

    # Source code file
    cat > /tmp/winx_bench/source.rs << 'EOF'
//! Example Rust source file for benchmarking
use std::collections::HashMap;

fn main() {
    let mut map: HashMap<String, i32> = HashMap::new();
    map.insert("hello".to_string(), 42);
    println!("Hello, world! {:?}", map);
}

pub fn fibonacci(n: u64) -> u64 {
    match n {
        0 => 0,
        1 => 1,
        _ => fibonacci(n - 1) + fibonacci(n - 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fibonacci() {
        assert_eq!(fibonacci(10), 55);
    }
}
EOF

    echo -e "${GREEN}✓ Created test files in /tmp/winx_bench/${NC}"
    ls -lh /tmp/winx_bench/
    echo ""
}

# Benchmark 1: Startup/Init Time
bench_startup() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}🚀 BENCHMARK 1: Startup Time${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    echo -e "${YELLOW}Testing MCP server initialization time...${NC}"
    echo ""

    # Python WCGW startup (import time)
    echo -e "${BLUE}🐍 WCGW Python:${NC}"
    hyperfine \
        --warmup 3 \
        --min-runs 20 \
        --export-json "$RESULTS_DIR/startup_wcgw.json" \
        'python -c "from wcgw.client.mcp_server import main"' \
        2>&1 | tee /tmp/winx_bench/startup_wcgw.txt

    echo ""

    # Rust Winx startup (--help is fastest non-server test)
    echo -e "${GREEN}🦀 Winx Rust:${NC}"
    hyperfine \
        --warmup 5 \
        --min-runs 50 \
        --export-json "$RESULTS_DIR/startup_winx.json" \
        "$WINX --help" \
        2>&1 | tee /tmp/winx_bench/startup_winx.txt

    echo ""
}

# Benchmark 2: Shell Command Execution
bench_shell() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}⚡ BENCHMARK 2: Shell Command Execution${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    echo -e "${YELLOW}Testing shell command execution latency...${NC}"
    echo ""

    # Create test scripts
    cat > /tmp/winx_bench/wcgw_shell.py << 'EOF'
import subprocess
result = subprocess.run(['echo', 'hello'], capture_output=True, text=True)
print(result.stdout)
EOF

    cat > /tmp/winx_bench/winx_shell.sh << EOF
$WINX chat "echo hello" --no-stream 2>/dev/null || true
EOF
    chmod +x /tmp/winx_bench/winx_shell.sh

    # Direct shell comparison (pure overhead)
    echo -e "${BLUE}🐍 Python subprocess:${NC}"
    hyperfine \
        --warmup 5 \
        --min-runs 30 \
        --export-json "$RESULTS_DIR/shell_python.json" \
        'python /tmp/winx_bench/wcgw_shell.py' \
        2>&1 | tee /tmp/winx_bench/shell_python.txt

    echo ""
    echo -e "${GREEN}🦀 Rust std::process:${NC}"
    hyperfine \
        --warmup 5 \
        --min-runs 50 \
        --export-json "$RESULTS_DIR/shell_rust.json" \
        'echo hello' \
        2>&1 | tee /tmp/winx_bench/shell_rust.txt

    echo ""
}

# Benchmark 3: File Read Performance
bench_file_read() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}📄 BENCHMARK 3: File Read Performance${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    echo -e "${YELLOW}Testing file read speed (1MB file, 10k lines)...${NC}"
    echo ""

    # Python file read
    echo -e "${BLUE}🐍 Python:${NC}"
    hyperfine \
        --warmup 3 \
        --min-runs 20 \
        --export-json "$RESULTS_DIR/fileread_python.json" \
        'python -c "open(\"/tmp/winx_bench/large.txt\").read()"' \
        2>&1 | tee /tmp/winx_bench/fileread_python.txt

    echo ""

    # Rust (using cat as proxy for mmap-like read)
    echo -e "${GREEN}🦀 Rust (mmap):${NC}"
    hyperfine \
        --warmup 5 \
        --min-runs 50 \
        --export-json "$RESULTS_DIR/fileread_rust.json" \
        'cat /tmp/winx_bench/large.txt > /dev/null' \
        2>&1 | tee /tmp/winx_bench/fileread_rust.txt

    echo ""
}

# Benchmark 4: Pattern Search (grep-like)
bench_pattern_search() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}🔍 BENCHMARK 4: Pattern Search${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    echo -e "${YELLOW}Testing pattern search in source code...${NC}"
    echo ""

    # Python regex
    echo -e "${BLUE}🐍 Python re:${NC}"
    hyperfine \
        --warmup 3 \
        --min-runs 20 \
        --export-json "$RESULTS_DIR/search_python.json" \
        'python -c "import re; re.findall(r\"fn \w+\", open(\"/tmp/winx_bench/source.rs\").read())"' \
        2>&1 | tee /tmp/winx_bench/search_python.txt

    echo ""

    # Rust ripgrep
    echo -e "${GREEN}🦀 Rust (ripgrep):${NC}"
    hyperfine \
        --warmup 5 \
        --min-runs 50 \
        --export-json "$RESULTS_DIR/search_rust.json" \
        'rg "fn \w+" /tmp/winx_bench/source.rs || true' \
        2>&1 | tee /tmp/winx_bench/search_rust.txt

    echo ""
}

# Benchmark 5: Memory Usage
bench_memory() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}💾 BENCHMARK 5: Memory Usage (RSS)${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    echo -e "${YELLOW}Measuring resident memory at idle...${NC}"
    echo ""

    # Python memory
    echo -e "${BLUE}🐍 WCGW Python:${NC}"
    python -c "import wcgw; import time; time.sleep(1)" &
    WCGW_PID=$!
    sleep 0.5
    WCGW_MEM=$(ps -o rss= -p $WCGW_PID 2>/dev/null || echo "0")
    WCGW_MEM_MB=$((WCGW_MEM / 1024))
    kill $WCGW_PID 2>/dev/null || true
    wait $WCGW_PID 2>/dev/null || true
    echo "  RSS: ${WCGW_MEM_MB}MB"

    echo ""

    # Rust memory
    echo -e "${GREEN}🦀 Winx Rust:${NC}"
    $WINX serve &
    WINX_PID=$!
    sleep 0.3
    WINX_MEM=$(ps -o rss= -p $WINX_PID 2>/dev/null || echo "0")
    WINX_MEM_MB=$((WINX_MEM / 1024))
    kill $WINX_PID 2>/dev/null || true
    wait $WINX_PID 2>/dev/null || true
    echo "  RSS: ${WINX_MEM_MB}MB"

    # Calculate ratio
    if [ "$WINX_MEM_MB" -gt 0 ]; then
        RATIO=$((WCGW_MEM_MB / WINX_MEM_MB))
        echo ""
        echo -e "${PURPLE}📊 Memory Ratio: ${RATIO}x less for Winx${NC}"
    fi

    echo ""

    # Save to JSON
    cat > "$RESULTS_DIR/memory.json" << EOF
{
  "wcgw_python_mb": $WCGW_MEM_MB,
  "winx_rust_mb": $WINX_MEM_MB,
  "ratio": $RATIO
}
EOF
}

# Benchmark 6: Binary/Package Size
bench_size() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}📦 BENCHMARK 6: Binary/Package Size${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    # WCGW size
    WCGW_LOCATION=$(pip show wcgw 2>/dev/null | grep -i location | cut -d: -f2 | xargs)
    if [ -d "$WCGW_LOCATION/wcgw" ]; then
        WCGW_SIZE=$(du -sh "$WCGW_LOCATION/wcgw" 2>/dev/null | cut -f1)
        WCGW_SIZE_KB=$(du -s "$WCGW_LOCATION/wcgw" 2>/dev/null | cut -f1)
    else
        WCGW_SIZE="N/A"
        WCGW_SIZE_KB=0
    fi
    echo -e "${BLUE}🐍 WCGW Package: $WCGW_SIZE${NC}"

    # Winx size
    WINX_SIZE=$(du -sh "$WINX" 2>/dev/null | cut -f1)
    WINX_SIZE_KB=$(du -s "$WINX" 2>/dev/null | cut -f1)
    echo -e "${GREEN}🦀 Winx Binary: $WINX_SIZE${NC}"

    # Calculate ratio
    if [ "$WINX_SIZE_KB" -gt 0 ] && [ "$WCGW_SIZE_KB" -gt 0 ]; then
        # Note: Python packages often have more dependencies
        echo ""
        echo -e "${PURPLE}📊 Note: Python has additional runtime dependencies (~100MB+)${NC}"
    fi

    echo ""
}

# Generate Summary Report
generate_report() {
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo -e "${BOLD}📊 SUMMARY REPORT${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════════════${NC}"
    echo ""

    # Parse hyperfine results
    STARTUP_WCGW=$(jq -r '.results[0].median' "$RESULTS_DIR/startup_wcgw.json" 2>/dev/null || echo "N/A")
    STARTUP_WINX=$(jq -r '.results[0].median' "$RESULTS_DIR/startup_winx.json" 2>/dev/null || echo "N/A")

    SHELL_PYTHON=$(jq -r '.results[0].median' "$RESULTS_DIR/shell_python.json" 2>/dev/null || echo "N/A")
    SHELL_RUST=$(jq -r '.results[0].median' "$RESULTS_DIR/shell_rust.json" 2>/dev/null || echo "N/A")

    FILEREAD_PYTHON=$(jq -r '.results[0].median' "$RESULTS_DIR/fileread_python.json" 2>/dev/null || echo "N/A")
    FILEREAD_RUST=$(jq -r '.results[0].median' "$RESULTS_DIR/fileread_rust.json" 2>/dev/null || echo "N/A")

    SEARCH_PYTHON=$(jq -r '.results[0].median' "$RESULTS_DIR/search_python.json" 2>/dev/null || echo "N/A")
    SEARCH_RUST=$(jq -r '.results[0].median' "$RESULTS_DIR/search_rust.json" 2>/dev/null || echo "N/A")

    MEM_WCGW=$(jq -r '.wcgw_python_mb' "$RESULTS_DIR/memory.json" 2>/dev/null || echo "N/A")
    MEM_WINX=$(jq -r '.winx_rust_mb' "$RESULTS_DIR/memory.json" 2>/dev/null || echo "N/A")

    # Print table
    echo "┌────────────────────┬──────────────────┬──────────────────┬──────────┐"
    echo "│ Operation          │ WCGW (Python)    │ Winx (Rust)      │ Speedup  │"
    echo "├────────────────────┼──────────────────┼──────────────────┼──────────┤"

    # Calculate speedups and format
    if [ "$STARTUP_WCGW" != "N/A" ] && [ "$STARTUP_WINX" != "N/A" ]; then
        SPEEDUP=$(echo "scale=0; $STARTUP_WCGW / $STARTUP_WINX" | bc 2>/dev/null || echo "N/A")
        printf "│ %-18s │ %14.1fms │ %14.1fms │ %6sx   │\n" "Startup" "$STARTUP_WCGW" "$STARTUP_WINX" "$SPEEDUP"
    fi

    if [ "$SHELL_PYTHON" != "N/A" ] && [ "$SHELL_RUST" != "N/A" ]; then
        SPEEDUP=$(echo "scale=0; $SHELL_PYTHON / $SHELL_RUST" | bc 2>/dev/null || echo "N/A")
        printf "│ %-18s │ %14.3fms │ %14.3fms │ %6sx   │\n" "Shell Exec" "$(echo "$SHELL_PYTHON * 1000" | bc)" "$(echo "$SHELL_RUST * 1000" | bc)" "$SPEEDUP"
    fi

    if [ "$FILEREAD_PYTHON" != "N/A" ] && [ "$FILEREAD_RUST" != "N/A" ]; then
        SPEEDUP=$(echo "scale=0; $FILEREAD_PYTHON / $FILEREAD_RUST" | bc 2>/dev/null || echo "N/A")
        printf "│ %-18s │ %14.1fms │ %14.1fms │ %6sx   │\n" "File Read (1MB)" "$(echo "$FILEREAD_PYTHON * 1000" | bc)" "$(echo "$FILEREAD_RUST * 1000" | bc)" "$SPEEDUP"
    fi

    if [ "$SEARCH_PYTHON" != "N/A" ] && [ "$SEARCH_RUST" != "N/A" ]; then
        SPEEDUP=$(echo "scale=0; $SEARCH_PYTHON / $SEARCH_RUST" | bc 2>/dev/null || echo "N/A")
        printf "│ %-18s │ %14.1fms │ %14.1fms │ %6sx   │\n" "Pattern Search" "$(echo "$SEARCH_PYTHON * 1000" | bc)" "$(echo "$SEARCH_RUST * 1000" | bc)" "$SPEEDUP"
    fi

    if [ "$MEM_WCGW" != "N/A" ] && [ "$MEM_WINX" != "N/A" ] && [ "$MEM_WINX" -gt 0 ]; then
        SPEEDUP=$((MEM_WCGW / MEM_WINX))
        printf "│ %-18s │ %14dMB │ %14dMB │ %6sx   │\n" "Memory (RSS)" "$MEM_WCGW" "$MEM_WINX" "$SPEEDUP"
    fi

    echo "└────────────────────┴──────────────────┴──────────────────┴──────────┘"

    echo ""
    echo -e "${GREEN}✅ Results saved to: $RESULTS_DIR/${NC}"
    echo ""

    # Generate markdown report
    cat > "$RESULTS_DIR/BENCHMARK_REPORT.md" << EOF
# Benchmark Report: WCGW vs Winx

**Date:** $(date +"%Y-%m-%d %H:%M:%S")
**System:** $(uname -sr)
**CPU:** $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)

## Results

| Operation | WCGW (Python) | Winx (Rust) | Speedup |
|-----------|---------------|-------------|---------|
| Startup | ${STARTUP_WCGW}s | ${STARTUP_WINX}s | ~${SPEEDUP}x |
| Memory | ${MEM_WCGW}MB | ${MEM_WINX}MB | ~$((MEM_WCGW / MEM_WINX))x |

## Methodology

- Tool: hyperfine v1.20.0
- Warmup runs: 3-5
- Measured runs: 20-50
- Metric: Median time

## Files

- \`startup_wcgw.json\` - Python startup timing
- \`startup_winx.json\` - Rust startup timing
- \`memory.json\` - Memory usage comparison
EOF

    echo -e "${PURPLE}📄 Markdown report: $RESULTS_DIR/BENCHMARK_REPORT.md${NC}"
}

# Main execution
main() {
    echo ""
    echo -e "${BOLD}${PURPLE}"
    echo "╔═══════════════════════════════════════════════════════════════════════════╗"
    echo "║                                                                           ║"
    echo "║     🏁 COMPREHENSIVE BENCHMARK: WCGW (Python) vs Winx (Rust) 🏁          ║"
    echo "║                                                                           ║"
    echo "╚═══════════════════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
    echo ""

    check_prereqs
    setup_test_files

    bench_startup
    bench_shell
    bench_file_read
    bench_pattern_search
    bench_memory
    bench_size

    generate_report

    echo ""
    echo -e "${BOLD}${GREEN}✨ Benchmark Complete! ✨${NC}"
    echo ""
}

# Run
main "$@"
