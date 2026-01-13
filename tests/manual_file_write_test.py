#!/usr/bin/env python3
"""
Test suite for Winx MCP FileWriteOrEdit functionality.
Tests: create new file, edit with SEARCH/REPLACE, whitelist enforcement, multiple blocks.
"""

import json
import subprocess
import sys
import time
from pathlib import Path

# Colors for output
GREEN = "\033[92m"
RED = "\033[91m"
YELLOW = "\033[93m"
RESET = "\033[0m"

WINX_BINARY = "/home/mrootx/mcp/winx-code-agent/target/release/winx-code-agent"
TEST_DIR = Path("/tmp/winx-test")
THREAD_ID = "i2238"


class WinxMCPClient:
    """Simple MCP client for testing Winx."""

    def __init__(self):
        self.request_id = 0
        self.process = None

    def start(self):
        """Start the Winx MCP server process."""
        self.process = subprocess.Popen(
            [WINX_BINARY],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1
        )

        # Step 1: Send initialize request
        self._send_request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "1.0.0"}
        })
        init_response = self._read_response()

        # Step 2: Send initialized notification (required by MCP protocol)
        self._send_notification("notifications/initialized", {})

        time.sleep(0.1)  # Give server time to process

        return init_response

    def stop(self):
        """Stop the Winx MCP server process."""
        if self.process:
            try:
                self.process.terminate()
                self.process.wait(timeout=5)
            except:
                self.process.kill()

    def _send_request(self, method: str, params: dict) -> int:
        """Send a JSON-RPC request to the server."""
        self.request_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": self.request_id,
            "method": method,
            "params": params
        }
        line = json.dumps(request) + "\n"
        self.process.stdin.write(line)
        self.process.stdin.flush()
        return self.request_id

    def _send_notification(self, method: str, params: dict):
        """Send a JSON-RPC notification (no id, no response expected)."""
        notification = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }
        line = json.dumps(notification) + "\n"
        self.process.stdin.write(line)
        self.process.stdin.flush()

    def _read_response(self, timeout: float = 30.0) -> dict:
        """Read a JSON-RPC response from the server."""
        import select
        import os

        # Use select to wait for data with timeout
        if hasattr(select, 'select'):
            fd = self.process.stdout.fileno()
            ready, _, _ = select.select([fd], [], [], timeout)
            if not ready:
                # Drain stderr for debugging
                stderr = ""
                try:
                    fd_err = self.process.stderr.fileno()
                    ready_err, _, _ = select.select([fd_err], [], [], 0.1)
                    if ready_err:
                        import fcntl
                        flags = fcntl.fcntl(fd_err, fcntl.F_GETFL)
                        fcntl.fcntl(fd_err, fcntl.F_SETFL, flags | os.O_NONBLOCK)
                        try:
                            stderr = self.process.stderr.read(8192)
                        except:
                            pass
                except:
                    pass
                return {"error": f"Timeout waiting for response. Stderr: {stderr}"}

        line = self.process.stdout.readline()
        if not line:
            # Check stderr for error messages
            stderr = ""
            try:
                import select
                if hasattr(select, 'select'):
                    fd = self.process.stderr.fileno()
                    ready, _, _ = select.select([fd], [], [], 0.1)
                    if ready:
                        import fcntl
                        flags = fcntl.fcntl(fd, fcntl.F_GETFL)
                        fcntl.fcntl(fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
                        try:
                            stderr = self.process.stderr.read(8192)
                        except:
                            pass
            except:
                pass
            return {"error": f"No response from server. Stderr: {stderr}"}
        try:
            return json.loads(line)
        except json.JSONDecodeError as e:
            return {"error": f"JSON decode error: {e}", "raw": line}

    def call_tool(self, tool_name: str, arguments: dict) -> dict:
        """Call an MCP tool and return the result."""
        self._send_request("tools/call", {
            "name": tool_name,
            "arguments": arguments
        })
        return self._read_response()


def print_test(name: str, passed: bool, details: str = ""):
    """Print test result."""
    status = f"{GREEN}PASS{RESET}" if passed else f"{RED}FAIL{RESET}"
    print(f"[{status}] {name}")
    if details and not passed:
        print(f"       {details[:500]}")


def test_initialize(client: WinxMCPClient) -> bool:
    """Test 1: Initialize the shell environment."""
    print(f"\n{YELLOW}=== Test 1: Initialize ==={RESET}")

    result = client.call_tool("Initialize", {
        "type": "first_call",
        "any_workspace_path": str(TEST_DIR),
        "initial_files_to_read": [],
        "task_id_to_resume": "",
        "mode_name": "wcgw",
        "thread_id": THREAD_ID,
        "code_writer_config": None
    })

    if "error" in result:
        print_test("Initialize shell", False, str(result.get("error")))
        return False

    content = result.get("result", {}).get("content", [])
    if content and len(content) > 0:
        text = content[0].get("text", "")
        passed = "initialized" in text.lower() or "mode: wcgw" in text.lower() or "shell" in text.lower() or "workspace" in text.lower()
        print_test("Initialize shell", passed, text[:300] if not passed else "")
        print(f"       Response: {text[:200]}...")
        return passed

    print_test("Initialize shell", False, f"No content in response: {result}")
    return False


def test_create_new_file(client: WinxMCPClient) -> bool:
    """Test 2: Create a new file with percentage > 50 (full write)."""
    print(f"\n{YELLOW}=== Test 2: Create New File (percentage > 50) ==={RESET}")

    test_file = TEST_DIR / "new_file.py"
    content = '''#!/usr/bin/env python3
"""A test Python file."""

def hello():
    """Say hello."""
    print("Hello, World!")

def add(a, b):
    """Add two numbers."""
    return a + b

if __name__ == "__main__":
    hello()
'''

    # Remove file if exists
    if test_file.exists():
        test_file.unlink()

    result = client.call_tool("FileWriteOrEdit", {
        "file_path": str(test_file),
        "percentage_to_change": 100,
        "text_or_search_replace_blocks": content,
        "thread_id": THREAD_ID
    })

    if "error" in result:
        print_test("Create new file", False, str(result.get("error")))
        return False

    # Give time for async whitelist update
    time.sleep(0.5)

    # Verify file was created
    if not test_file.exists():
        print_test("Create new file", False, "File was not created")
        return False

    actual_content = test_file.read_text()
    passed = "def hello():" in actual_content and "def add(a, b):" in actual_content
    print_test("Create new file", passed)

    return passed


def test_whitelist_enforcement(client: WinxMCPClient) -> bool:
    """Test 3: Try to edit a file without reading it first (should fail)."""
    print(f"\n{YELLOW}=== Test 3: Whitelist Enforcement ==={RESET}")

    # Create a file directly (bypassing Winx)
    unread_file = TEST_DIR / "unread_file.txt"
    unread_file.write_text("Original content\nLine 2\nLine 3\n")

    # Try to edit it without reading first
    result = client.call_tool("FileWriteOrEdit", {
        "file_path": str(unread_file),
        "percentage_to_change": 30,
        "text_or_search_replace_blocks": '''<<<<<<< SEARCH
Original content
=======
Modified content
>>>>>>> REPLACE''',
        "thread_id": THREAD_ID
    })

    # Should fail with whitelist error
    error_data = result.get("error", {})
    error_msg = ""
    if isinstance(error_data, dict):
        error_msg = error_data.get("message", str(error_data))
    else:
        error_msg = str(error_data)

    # Check if error mentions needing to read first
    if error_msg:
        error_text = error_msg.lower()
        passed = "read" in error_text or "whitelist" in error_text
        print_test("Whitelist blocks unread file edit", passed, error_msg[:300])
        return passed

    # If no error, the whitelist is not working
    print_test("Whitelist blocks unread file edit", False, f"Edit succeeded without reading file first: {result}")
    return False


def test_read_then_edit(client: WinxMCPClient) -> bool:
    """Test 4: Read a file, then edit it with SEARCH/REPLACE."""
    print(f"\n{YELLOW}=== Test 4: Read Then Edit (SEARCH/REPLACE) ==={RESET}")

    # Use the file we created in test 2
    test_file = TEST_DIR / "new_file.py"

    if not test_file.exists():
        print_test("File exists from previous test", False)
        return False

    # First, read the file - this should update the whitelist with current hash
    read_result = client.call_tool("ReadFiles", {
        "file_paths": [str(test_file)]
    })

    if "error" in read_result:
        print_test("Read file before edit", False, str(read_result.get("error")))
        return False

    print_test("Read file before edit", True)

    # Give time for async whitelist update
    time.sleep(0.5)

    # Now edit with SEARCH/REPLACE
    result = client.call_tool("FileWriteOrEdit", {
        "file_path": str(test_file),
        "percentage_to_change": 20,
        "text_or_search_replace_blocks": '''<<<<<<< SEARCH
def hello():
    """Say hello."""
    print("Hello, World!")
=======
def hello(name="World"):
    """Say hello to someone."""
    print(f"Hello, {name}!")
>>>>>>> REPLACE''',
        "thread_id": THREAD_ID
    })

    if "error" in result:
        error_data = result.get("error", {})
        error_msg = error_data.get("message", str(error_data)) if isinstance(error_data, dict) else str(error_data)
        print_test("Edit with SEARCH/REPLACE", False, error_msg)
        return False

    # Verify the edit
    actual_content = test_file.read_text()
    passed = 'def hello(name="World"):' in actual_content and 'print(f"Hello, {name}!")' in actual_content
    print_test("Edit with SEARCH/REPLACE", passed, actual_content[:300] if not passed else "")

    # Wait for whitelist update after edit
    time.sleep(0.5)

    return passed


def test_multiple_search_replace_blocks(client: WinxMCPClient) -> bool:
    """Test 5: Edit with multiple SEARCH/REPLACE blocks."""
    print(f"\n{YELLOW}=== Test 5: Multiple SEARCH/REPLACE Blocks ==={RESET}")

    test_file = TEST_DIR / "new_file.py"

    # Wait for previous async operations to complete
    time.sleep(0.5)

    # First, read the file again (state may have changed from previous edit)
    read_result = client.call_tool("ReadFiles", {
        "file_paths": [str(test_file)]
    })

    if "error" in read_result:
        print_test("Read file before multi-block edit", False, str(read_result.get("error")))
        return False

    # Give time for async whitelist update
    time.sleep(0.5)

    # Edit with multiple SEARCH/REPLACE blocks
    result = client.call_tool("FileWriteOrEdit", {
        "file_path": str(test_file),
        "percentage_to_change": 30,
        "text_or_search_replace_blocks": '''<<<<<<< SEARCH
def add(a, b):
    """Add two numbers."""
    return a + b
=======
def add(a, b):
    """Add two numbers together."""
    result = a + b
    return result

def subtract(a, b):
    """Subtract b from a."""
    return a - b
>>>>>>> REPLACE

<<<<<<< SEARCH
if __name__ == "__main__":
    hello()
=======
if __name__ == "__main__":
    hello("Winx")
    print(f"2 + 3 = {add(2, 3)}")
    print(f"5 - 2 = {subtract(5, 2)}")
>>>>>>> REPLACE''',
        "thread_id": THREAD_ID
    })

    if "error" in result:
        error_data = result.get("error", {})
        error_msg = error_data.get("message", str(error_data)) if isinstance(error_data, dict) else str(error_data)
        print_test("Multiple SEARCH/REPLACE blocks", False, error_msg)
        return False

    # Verify the edits
    actual_content = test_file.read_text()
    checks = [
        '"Add two numbers together."' in actual_content,
        'def subtract(a, b):' in actual_content,
        'hello("Winx")' in actual_content,
        'subtract(5, 2)' in actual_content
    ]

    passed = all(checks)
    print_test("Multiple SEARCH/REPLACE blocks", passed)

    if passed:
        print(f"\n{YELLOW}Final file content:{RESET}")
        print(actual_content)

    return passed


def test_syntax_error_detection(client: WinxMCPClient) -> bool:
    """Test 6: Create a file with syntax errors and check if warning is returned."""
    print(f"\n{YELLOW}=== Test 6: Syntax Error Detection ==={RESET}")

    test_file = TEST_DIR / "syntax_error.py"
    content = '''#!/usr/bin/env python3
def broken(:
    print("This has a syntax error"
'''

    result = client.call_tool("FileWriteOrEdit", {
        "file_path": str(test_file),
        "percentage_to_change": 100,
        "text_or_search_replace_blocks": content,
        "thread_id": THREAD_ID
    })

    response_text = ""
    if "result" in result:
        content_list = result.get("result", {}).get("content", [])
        if content_list:
            response_text = content_list[0].get("text", "")

    # Check if syntax error warning is included
    passed = "syntax" in response_text.lower() or "error" in response_text.lower()
    print_test("Syntax error detection", passed, response_text[:300] if passed else "No syntax warning in response")

    return passed


def main():
    """Run all tests."""
    print(f"{YELLOW}========================================{RESET}")
    print(f"{YELLOW}  Winx FileWriteOrEdit Test Suite{RESET}")
    print(f"{YELLOW}  Thread ID: {THREAD_ID}{RESET}")
    print(f"{YELLOW}  Test Dir: {TEST_DIR}{RESET}")
    print(f"{YELLOW}========================================{RESET}")

    # Ensure test directory exists
    TEST_DIR.mkdir(parents=True, exist_ok=True)

    # Clean up old test files
    for f in TEST_DIR.glob("*.py"):
        f.unlink()
    for f in TEST_DIR.glob("*.txt"):
        if f.name not in ["test.txt"]:
            f.unlink()

    client = WinxMCPClient()

    try:
        # Start the MCP server
        print(f"\n{YELLOW}Starting Winx MCP server...{RESET}")
        init_response = client.start()
        print(f"Init response: {json.dumps(init_response, indent=2)[:500]}")

        results = []

        # Run tests
        results.append(("Initialize", test_initialize(client)))
        results.append(("Create New File", test_create_new_file(client)))
        results.append(("Whitelist Enforcement", test_whitelist_enforcement(client)))
        results.append(("Read Then Edit", test_read_then_edit(client)))
        results.append(("Multiple SEARCH/REPLACE", test_multiple_search_replace_blocks(client)))
        results.append(("Syntax Error Detection", test_syntax_error_detection(client)))

        # Summary
        print(f"\n{YELLOW}========================================{RESET}")
        print(f"{YELLOW}  Test Summary{RESET}")
        print(f"{YELLOW}========================================{RESET}")

        passed = sum(1 for _, p in results if p)
        total = len(results)

        for name, p in results:
            status = f"{GREEN}PASS{RESET}" if p else f"{RED}FAIL{RESET}"
            print(f"  [{status}] {name}")

        print(f"\n  {passed}/{total} tests passed")

        if passed == total:
            print(f"\n{GREEN}All tests passed!{RESET}")
            return 0
        else:
            print(f"\n{RED}Some tests failed.{RESET}")
            return 1

    finally:
        client.stop()


if __name__ == "__main__":
    sys.exit(main())
