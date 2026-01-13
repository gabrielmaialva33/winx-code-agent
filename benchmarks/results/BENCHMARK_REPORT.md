# Benchmark Report: WCGW (Python) vs Winx (Rust)

**Date:** 2026-01-13
**System:** i9-13900K | RTX 4090 24GB | WSL2
**Tool:** hyperfine 1.20.0

## Results Summary

| Operation | WCGW (Python) | Winx (Rust) | Speedup |
|-----------|---------------|-------------|---------|
| **Startup** | ~2500ms | 3ms | 🚀 **833x** |
| **Shell Exec** | 56ms | <1ms | 🚀 **56x** |
| **File Read (1MB)** | 48ms | 0.45ms | 🚀 **107x** |
| **Pattern Search** | 50ms | 14ms | 🚀 **3.5x** |
| **Memory (RSS)** | 71MB | ~5MB | 🚀 **14x** |

## Detailed Results

### 1. Startup Time

```
WCGW Python: ~2500ms (Python interpreter + imports)
Winx Rust:   3ms (native binary, zero dependencies)

Speedup: 833x faster
```

### 2. Shell Command Execution

```
Python subprocess:  56.0ms ± 1.6ms
Rust std::process:  <1ms

Speedup: 56x faster
```

### 3. File Read (1MB file)

```
Python open/read:   47.6ms ± 1.2ms
Rust mmap:          0.45ms ± 0.1ms

Speedup: 107x faster
```

### 4. Pattern Search (regex)

```
Python re module:   50.2ms ± 0.9ms
Rust ripgrep:       14.4ms ± 5.8ms

Speedup: 3.5x faster
```

### 5. Memory Usage

```
WCGW Python: 71MB RSS (+ Python runtime)
Winx Rust:   ~5MB RSS (standalone binary)

Reduction: 14x less memory
```

## Methodology

- **Tool:** hyperfine 1.20.0
- **Warmup:** 3-5 runs
- **Measured:** 20-50 runs per benchmark
- **Metric:** Median time (less sensitive to outliers)
- **Environment:** Quiet system, no background processes

## Why Rust is Faster

1. **No Interpreter Overhead** - Compiled binary vs Python interpreter
2. **Zero-Copy File IO** - mmap vs Python's buffered reads
3. **Native PTY** - Direct terminal emulation vs subprocess
4. **No GIL** - True parallelism (async/await)
5. **Smaller Footprint** - 8.8MB binary vs 71MB+ runtime

## Files

- `startup_winx.json` - Rust startup timing
- `shell_python.json` / `shell_rust.json` - Shell execution
- `fileread_python.json` / `fileread_rust.json` - File I/O
- `search_python.json` / `search_rust.json` - Regex search
- `memory.json` - Memory comparison
