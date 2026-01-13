#!/usr/bin/env python3
"""
═══════════════════════════════════════════════════════════════════════════════
🏁 MCP TOOLS BENCHMARK: WCGW vs Winx - FAIR 1:1 COMPARISON
═══════════════════════════════════════════════════════════════════════════════

Tests EACH MCP tool via JSON-RPC stdin/stdout protocol.
Same input, same measurement, fair comparison.

Methodology:
- Start MCP server (WCGW or Winx)
- Send JSON-RPC requests via stdin
- Measure response time
- Compare 1:1
"""

import json
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from statistics import mean, median
from typing import List, Dict, Any, Optional, Tuple
import tempfile
import shutil

# ═══════════════════════════════════════════════════════════════════════════════
# Configuration
# ═══════════════════════════════════════════════════════════════════════════════

PROJECT_ROOT = Path(__file__).parent.parent
WINX_BINARY = PROJECT_ROOT / "target" / "release" / "winx-code-agent"
RESULTS_DIR = PROJECT_ROOT / "benchmarks" / "results"

WARMUP_RUNS = 2
MEASURED_RUNS = 10

# Test directory
TEST_DIR = Path(tempfile.mkdtemp(prefix="mcp_bench_"))


class Colors:
    RED = '\033[0;31m'
    GREEN = '\033[0;32m'
    YELLOW = '\033[0;33m'
    BLUE = '\033[0;34m'
    PURPLE = '\033[0;35m'
    CYAN = '\033[0;36m'
    NC = '\033[0m'
    BOLD = '\033[1m'


@dataclass
class ToolResult:
    """Result for a single tool test"""
    tool_name: str
    scenario: str
    wcgw_times_ms: List[float] = field(default_factory=list)
    winx_times_ms: List[float] = field(default_factory=list)
    wcgw_success: int = 0
    winx_success: int = 0

    @property
    def wcgw_median(self) -> float:
        return median(self.wcgw_times_ms) if self.wcgw_times_ms else 0

    @property
    def winx_median(self) -> float:
        return median(self.winx_times_ms) if self.winx_times_ms else 0

    @property
    def speedup(self) -> float:
        if self.winx_median > 0:
            return self.wcgw_median / self.winx_median
        return 0


# ═══════════════════════════════════════════════════════════════════════════════
# MCP JSON-RPC Protocol
# ═══════════════════════════════════════════════════════════════════════════════

def make_request(method: str, params: Dict[str, Any], request_id: int = 1) -> str:
    """Create a JSON-RPC 2.0 request"""
    return json.dumps({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params,
    })


def make_tool_call(tool_name: str, arguments: Dict[str, Any], request_id: int = 1) -> str:
    """Create a tools/call request"""
    return make_request("tools/call", {
        "name": tool_name,
        "arguments": arguments,
    }, request_id)


# ═══════════════════════════════════════════════════════════════════════════════
# Test Fixtures
# ═══════════════════════════════════════════════════════════════════════════════

def setup_test_files():
    """Create test files"""
    print(f"{Colors.CYAN}📁 Setting up test files in {TEST_DIR}...{Colors.NC}")

    # Small file (100 bytes)
    (TEST_DIR / "small.txt").write_text("Hello World!\n" * 8)

    # Medium file (10KB)
    (TEST_DIR / "medium.txt").write_text("Line of text.\n" * 700)

    # Large file (1MB)
    with open(TEST_DIR / "large.txt", "w") as f:
        for i in range(10000):
            f.write(f"Line {i}: Lorem ipsum dolor sit amet, consectetur.\n")

    # Source code file
    (TEST_DIR / "code.rs").write_text('''
use std::collections::HashMap;

fn main() {
    let mut map: HashMap<String, i32> = HashMap::new();
    map.insert("hello".to_string(), 42);
    println!("{:?}", map);
}

pub fn fibonacci(n: u64) -> u64 {
    match n {
        0 => 0,
        1 => 1,
        _ => fibonacci(n - 1) + fibonacci(n - 2),
    }
}
''')

    # File for edit
    (TEST_DIR / "config.txt").write_text('''
[settings]
debug = false
log_level = info
max_connections = 100
''')

    print(f"{Colors.GREEN}✓ Test files created{Colors.NC}")


def cleanup_test_files():
    """Remove test files"""
    try:
        shutil.rmtree(TEST_DIR)
    except Exception:
        pass


# ═══════════════════════════════════════════════════════════════════════════════
# Benchmarks
# ═══════════════════════════════════════════════════════════════════════════════

def measure_startup(cmd: List[str], name: str) -> Tuple[float, bool]:
    """Measure startup time"""
    start = time.perf_counter()
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        # Send initialize request
        init_req = make_request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "benchmark", "version": "1.0"}
        })
        proc.stdin.write((init_req + "\n").encode())
        proc.stdin.flush()

        # Wait for response (with timeout)
        proc.stdout.readline()
        end = time.perf_counter()

        proc.terminate()
        proc.wait(timeout=2)

        return (end - start) * 1000, True  # Convert to ms
    except Exception as e:
        print(f"  Error: {e}")
        return 0, False


def measure_tool_call(cmd: List[str], tool_name: str, args: Dict, warm: bool = False) -> Tuple[float, bool]:
    """Measure a single tool call"""
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        # 1. Initialize first
        init_req = make_request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bench", "version": "1.0"}
        })
        proc.stdin.write((init_req + "\n").encode())
        proc.stdin.flush()
        proc.stdout.readline()  # Read init response

        # 2. Initialize workspace
        ws_req = make_tool_call("Initialize", {
            "any_workspace_path": str(TEST_DIR),
            "mode_name": "wcgw"
        }, 2)
        proc.stdin.write((ws_req + "\n").encode())
        proc.stdin.flush()
        proc.stdout.readline()  # Read response

        # 3. Measure actual tool call
        start = time.perf_counter()

        tool_req = make_tool_call(tool_name, args, 3)
        proc.stdin.write((tool_req + "\n").encode())
        proc.stdin.flush()

        response = proc.stdout.readline()
        end = time.perf_counter()

        proc.terminate()
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()

        return (end - start) * 1000, True  # ms
    except Exception as e:
        if not warm:
            print(f"  Error: {e}")
        return 0, False


def run_benchmark(tool_name: str, scenario: str, args: Dict) -> ToolResult:
    """Run benchmark for a specific tool"""
    result = ToolResult(tool_name=tool_name, scenario=scenario)

    wcgw_cmd = ["wcgw"]
    winx_cmd = [str(WINX_BINARY), "serve"]

    # Warmup
    for _ in range(WARMUP_RUNS):
        measure_tool_call(wcgw_cmd, tool_name, args, warm=True)
        measure_tool_call(winx_cmd, tool_name, args, warm=True)

    # Measured runs - WCGW
    print(f"  {Colors.BLUE}🐍 WCGW:{Colors.NC} ", end="", flush=True)
    for _ in range(MEASURED_RUNS):
        elapsed, success = measure_tool_call(wcgw_cmd, tool_name, args)
        if success and elapsed > 0:
            result.wcgw_times_ms.append(elapsed)
            result.wcgw_success += 1
        print(".", end="", flush=True)

    if result.wcgw_times_ms:
        print(f" {result.wcgw_median:.1f}ms")
    else:
        print(" failed")

    # Measured runs - Winx
    print(f"  {Colors.GREEN}🦀 Winx:{Colors.NC} ", end="", flush=True)
    for _ in range(MEASURED_RUNS):
        elapsed, success = measure_tool_call(winx_cmd, tool_name, args)
        if success and elapsed > 0:
            result.winx_times_ms.append(elapsed)
            result.winx_success += 1
        print(".", end="", flush=True)

    if result.winx_times_ms:
        print(f" {result.winx_median:.1f}ms")
    else:
        print(" failed")

    return result


# ═══════════════════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════════════════

def main():
    print(f"\n{Colors.BOLD}{Colors.PURPLE}")
    print("╔═══════════════════════════════════════════════════════════════════════════╗")
    print("║                                                                           ║")
    print("║   🏁 MCP TOOLS BENCHMARK: WCGW vs Winx (1:1 Fair Comparison) 🏁          ║")
    print("║                                                                           ║")
    print("╚═══════════════════════════════════════════════════════════════════════════╝")
    print(f"{Colors.NC}\n")

    # Check prerequisites
    print(f"{Colors.CYAN}Checking prerequisites...{Colors.NC}")

    # Check WCGW
    try:
        result = subprocess.run(["wcgw", "--version"], capture_output=True, text=True)
        print(f"  {Colors.GREEN}✓ WCGW: {result.stdout.strip()}{Colors.NC}")
    except Exception:
        print(f"  {Colors.RED}✗ WCGW not found{Colors.NC}")
        return

    # Check Winx
    if not WINX_BINARY.exists():
        print(f"  {Colors.YELLOW}⚠ Building Winx...{Colors.NC}")
        subprocess.run(["cargo", "build", "--release"], cwd=PROJECT_ROOT)

    winx_size = WINX_BINARY.stat().st_size / (1024 * 1024)
    print(f"  {Colors.GREEN}✓ Winx: {winx_size:.1f}MB binary{Colors.NC}")

    print()
    setup_test_files()
    print()

    # Define test scenarios
    scenarios = [
        # ─────────────────────────────────────────────────────────────────
        # BashCommand Tests
        # ─────────────────────────────────────────────────────────────────
        ("BashCommand", "echo (simple)", {
            "action_json": json.dumps({"type": "command", "command": "echo hello"})
        }),
        ("BashCommand", "ls -la (list)", {
            "action_json": json.dumps({"type": "command", "command": f"ls -la {TEST_DIR}"})
        }),
        ("BashCommand", "pipeline (complex)", {
            "action_json": json.dumps({"type": "command", "command": f"cat {TEST_DIR}/large.txt | wc -l"})
        }),
        ("BashCommand", "multi-command", {
            "action_json": json.dumps({"type": "command", "command": "pwd && date && whoami"})
        }),

        # ─────────────────────────────────────────────────────────────────
        # ReadFiles Tests
        # ─────────────────────────────────────────────────────────────────
        ("ReadFiles", "small (100B)", {
            "file_paths": [str(TEST_DIR / "small.txt")]
        }),
        ("ReadFiles", "medium (10KB)", {
            "file_paths": [str(TEST_DIR / "medium.txt")]
        }),
        ("ReadFiles", "large (1MB)", {
            "file_paths": [str(TEST_DIR / "large.txt")]
        }),
        ("ReadFiles", "multiple files", {
            "file_paths": [
                str(TEST_DIR / "small.txt"),
                str(TEST_DIR / "medium.txt"),
                str(TEST_DIR / "code.rs"),
            ]
        }),

        # ─────────────────────────────────────────────────────────────────
        # FileWriteOrEdit Tests
        # ─────────────────────────────────────────────────────────────────
        ("FileWriteOrEdit", "write new file", {
            "file_path": str(TEST_DIR / "output.txt"),
            "percentage_to_change": 100,
            "file_content": "New content\n" * 100,
        }),
        ("FileWriteOrEdit", "SEARCH/REPLACE", {
            "file_path": str(TEST_DIR / "config.txt"),
            "percentage_to_change": 10,
            "file_content": """<<<<<<< SEARCH
debug = false
=======
debug = true
>>>>>>> REPLACE""",
        }),
    ]

    results: List[ToolResult] = []

    print(f"{Colors.CYAN}═══════════════════════════════════════════════════════════════════{Colors.NC}")
    print(f"{Colors.BOLD}Running {len(scenarios)} benchmark scenarios...{Colors.NC}")
    print(f"Warmup: {WARMUP_RUNS} | Measured: {MEASURED_RUNS}")
    print(f"{Colors.CYAN}═══════════════════════════════════════════════════════════════════{Colors.NC}\n")

    for i, (tool, scenario, args) in enumerate(scenarios, 1):
        print(f"{Colors.BOLD}[{i}/{len(scenarios)}] {tool}: {scenario}{Colors.NC}")

        result = run_benchmark(tool, scenario, args)
        results.append(result)

        if result.speedup > 0:
            color = Colors.GREEN if result.speedup >= 5 else Colors.YELLOW
            print(f"  {Colors.PURPLE}📊 Speedup: {color}{result.speedup:.1f}x{Colors.NC}")
        print()

    # ═══════════════════════════════════════════════════════════════════════════
    # Summary Table
    # ═══════════════════════════════════════════════════════════════════════════
    print(f"\n{Colors.CYAN}{'═' * 80}{Colors.NC}")
    print(f"{Colors.BOLD}📊 SUMMARY RESULTS{Colors.NC}")
    print(f"{Colors.CYAN}{'═' * 80}{Colors.NC}\n")

    print("┌──────────────────────────────────┬────────────┬────────────┬──────────┐")
    print("│ Tool / Scenario                  │ WCGW (ms)  │ Winx (ms)  │ Speedup  │")
    print("├──────────────────────────────────┼────────────┼────────────┼──────────┤")

    total_wcgw = 0
    total_winx = 0

    for r in results:
        name = f"{r.tool_name}: {r.scenario}"[:32]
        wcgw = r.wcgw_median
        winx = r.winx_median
        total_wcgw += wcgw
        total_winx += winx

        if r.speedup >= 10:
            speedup_str = f"{Colors.GREEN}🚀{r.speedup:>5.1f}x{Colors.NC}"
        elif r.speedup >= 5:
            speedup_str = f"{Colors.GREEN}{r.speedup:>7.1f}x{Colors.NC}"
        elif r.speedup >= 2:
            speedup_str = f"{Colors.YELLOW}{r.speedup:>7.1f}x{Colors.NC}"
        else:
            speedup_str = f"{r.speedup:>7.1f}x" if r.speedup > 0 else "   N/A  "

        print(f"│ {name:<32} │ {wcgw:>8.1f}   │ {winx:>8.1f}   │ {speedup_str} │")

    print("├──────────────────────────────────┼────────────┼────────────┼──────────┤")

    overall_speedup = total_wcgw / total_winx if total_winx > 0 else 0
    print(f"│ {'TOTAL':^32} │ {total_wcgw:>8.1f}   │ {total_winx:>8.1f}   │ {Colors.GREEN}{overall_speedup:>7.1f}x{Colors.NC} │")

    print("└──────────────────────────────────┴────────────┴────────────┴──────────┘")

    print(f"\n{Colors.BOLD}Overall Average Speedup: {Colors.GREEN}{overall_speedup:.1f}x faster with Winx{Colors.NC}")

    # ═══════════════════════════════════════════════════════════════════════════
    # Save Results
    # ═══════════════════════════════════════════════════════════════════════════
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    timestamp = time.strftime("%Y%m%d_%H%M%S")

    # JSON
    json_file = RESULTS_DIR / f"mcp_tools_{timestamp}.json"
    with open(json_file, "w") as f:
        json.dump({
            "timestamp": timestamp,
            "warmup": WARMUP_RUNS,
            "measured": MEASURED_RUNS,
            "results": [
                {
                    "tool": r.tool_name,
                    "scenario": r.scenario,
                    "wcgw_median_ms": r.wcgw_median,
                    "winx_median_ms": r.winx_median,
                    "speedup": r.speedup,
                }
                for r in results
            ],
            "overall_speedup": overall_speedup,
        }, f, indent=2)

    print(f"\n{Colors.GREEN}✓ Results saved to {json_file}{Colors.NC}")

    # Markdown for README
    md_content = f"""## ⚡ Benchmark Results

| Tool | Scenario | WCGW (ms) | Winx (ms) | Speedup |
|------|----------|-----------|-----------|---------|
"""
    for r in results:
        speedup = f"🚀 **{r.speedup:.0f}x**" if r.speedup >= 10 else f"**{r.speedup:.0f}x**"
        md_content += f"| {r.tool_name} | {r.scenario} | {r.wcgw_median:.1f} | {r.winx_median:.1f} | {speedup} |\n"

    md_content += f"\n**Overall: Winx is {overall_speedup:.0f}x faster than WCGW**\n"

    md_file = RESULTS_DIR / f"BENCHMARK_{timestamp}.md"
    with open(md_file, "w") as f:
        f.write(md_content)

    print(f"{Colors.GREEN}✓ Markdown saved to {md_file}{Colors.NC}")

    # Cleanup
    cleanup_test_files()

    print(f"\n{Colors.BOLD}{Colors.GREEN}✨ Benchmark Complete! ✨{Colors.NC}\n")


if __name__ == "__main__":
    main()
