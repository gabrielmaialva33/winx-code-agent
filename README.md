# Winx

A Rust implementation of WCGW (What Could Go Wrong) shell tools using the Model Context Protocol.

## Overview

Winx provides tools for working with a bash shell environment through MCP (Model Context Protocol).
It allows AI models like Claude to interact with a shell environment to execute commands and manage 
resources in a controlled and safe manner.

## Features

- **Initialize Tool**: Set up a bash environment with a specified workspace path and mode
  - Supports different operating modes (wcgw, architect, code_writer)
  - Configurable permissions for commands and file operations

- **BashCommand Tool**: Execute commands in the initialized bash environment
  - Supports various command types (Command, StatusCheck, SendText, SendSpecials, SendAscii)
  - Handles shell interaction and state management
  - Provides detailed output with status information

## Installation

```bash
# Clone the repository
git clone https://github.com/your-username/winx.git
cd winx

# Build the project
cargo build --release

# Run the binary
./target/release/winx
```

## Usage

Winx communicates using the Model Context Protocol via stdio. It's meant to be used as a tool
by AI models or MCP-compatible clients.

### Basic workflow:

1. Initialize the environment with the `initialize` tool
2. Execute commands with the `bashCommand` tool
3. Use the output to understand the state and results of commands

## Development

### Prerequisites

- Rust 1.70+
- Cargo

### Building

```bash
cargo build
```

### Testing

```bash
cargo test
```

## License

MIT
