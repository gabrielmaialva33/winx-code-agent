#!/bin/bash

# Winx Launcher Script
# This script helps with setting up and launching the Winx Code Agent

# Default settings
WINX_HOME="$(pwd)"
WINX_BIN="$WINX_HOME/target/release/winx-code-agent"
# shellcheck disable=SC2002
WINX_VERSION=$(cat "$WINX_HOME/Cargo.toml" | grep "version" | head -1 | cut -d '"' -f 2)
CLAUDE_CONFIG="$HOME/Library/Application Support/Claude/claude_desktop_config.json"
LOG_LEVEL="info"
DEBUG_MODE=false

# Colors for pretty output
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Print banner
echo -e "${BLUE}"
echo "✨ Ｗｉｎｘ Ａｇｅｎｔ Ｌａｕｎｃｈｅｒ ✨"
echo "Version: $WINX_VERSION"
echo -e "${NC}"

# Parse command line arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --debug)
      LOG_LEVEL="debug"
      shift
      ;;
    --debug-mode)
      DEBUG_MODE=true
      shift
      ;;
    --verbose)
      LOG_LEVEL="info"
      shift
      ;;
    --build)
      DO_BUILD=true
      shift
      ;;
    --install)
      DO_INSTALL=true
      shift
      ;;
    --help)
      echo "Usage: $0 [options]"
      echo "Options:"
      echo "  --debug       Enable debug logging"
      echo "  --debug-mode  Enable enhanced error reporting"
      echo "  --verbose     Enable verbose logging"
      echo "  --build       Build the agent before launching"
      echo "  --install     Install/update the agent in Claude config"
      echo "  --help        Show this help message"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      echo "Use --help for usage information"
      exit 1
      ;;
  esac
done

# Check if winx exists
if [ ! -d "$WINX_HOME" ]; then
  echo -e "${RED}Winx directory not found at $WINX_HOME${NC}"
  exit 1
fi

# Build if requested
if [ "$DO_BUILD" = true ]; then
  echo -e "${YELLOW}Building Winx agent...${NC}"
  cd "$WINX_HOME"
  cargo build --release
  
  if [ $? -ne 0 ]; then
    echo -e "${RED}Build failed!${NC}"
    exit 1
  fi
  
  echo -e "${GREEN}Build successful!${NC}"
fi

# Install in Claude config if requested
if [ "$DO_INSTALL" = true ]; then
  echo -e "${YELLOW}Installing Winx in Claude configuration...${NC}"
  
  # Check if Claude config exists
  if [ ! -f "$CLAUDE_CONFIG" ]; then
    # Create default config
    mkdir -p "$(dirname "$CLAUDE_CONFIG")"
    echo '{
  "mcpServers": {
    "winx": {
      "command": "'$WINX_BIN'",
      "args": [],
      "env": {
        "RUST_LOG": "'$LOG_LEVEL'"
      }
    }
  }
}' > "$CLAUDE_CONFIG"
    echo -e "${GREEN}Created new Claude configuration${NC}"
  else
    # Update existing config
    temp_file=$(mktemp)
    cat "$CLAUDE_CONFIG" | jq '.mcpServers.winx = {
      "command": "'$WINX_BIN'", 
      "args": [], 
      "env": {"RUST_LOG": "'$LOG_LEVEL'"}
    }' > "$temp_file"
    
    if [ $? -eq 0 ]; then
      mv "$temp_file" "$CLAUDE_CONFIG"
      echo -e "${GREEN}Updated Claude configuration${NC}"
    else
      echo -e "${RED}Failed to update Claude configuration${NC}"
      rm -f "$temp_file"
      exit 1
    fi
  fi
fi

# Check if Claude is running
if pgrep -x "Claude" > /dev/null; then
  echo -e "${YELLOW}Note: Claude is currently running. You may need to restart it for changes to take effect.${NC}"
fi

# Print status
echo -e "${GREEN}Winx agent is ready!${NC}"
echo "Binary: $WINX_BIN"
echo "Config: $CLAUDE_CONFIG"
echo "Log level: $LOG_LEVEL"
echo "Debug mode: $DEBUG_MODE"

# Optionally start the agent
if [ "$DEBUG_MODE" = true ]; then
  echo -e "${YELLOW}Starting Winx in debug mode...${NC}"
  "$WINX_BIN" --debug --debug-mode
else
  echo -e "${YELLOW}Starting Winx...${NC}"
  "$WINX_BIN" --$LOG_LEVEL
fi