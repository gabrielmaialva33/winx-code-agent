#!/usr/bin/env python3
"""
Debug test to check hash handling in Winx FileWriteOrEdit.
"""

import json
import subprocess
import sys
import time
from pathlib import Path

WINX_BINARY = "/home/mrootx/mcp/winx-code-agent/target/release/winx-code-agent"
TEST_DIR = Path("/tmp/winx-debug-test")
THREAD_ID = "debug-test-001"


class WinxMCPClient:
    def __init__(self):
        self.request_id = 0
        self.process = None

    def start(self):
        self.process = subprocess.Popen(
            [WINX_BINARY],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1
        )
        self._send_request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "1.0.0"}
        })
        init_response = self._read_response()
        self._send_notification("notifications/initialized", {})
        time.sleep(0.1)
        return init_response

    def stop(self):
        if self.process:
            try:
                self.process.terminate()
                self.process.wait(timeout=5)
            except:
                self.process.kill()

    def _send_request(self, method: str, params: dict) -> int:
        self.request_id += 1
        request = {"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params}
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        return self.request_id

    def _send_notification(self, method: str, params: dict):
        notification = {"jsonrpc": "2.0", "method": method, "params": params}
        self.process.stdin.write(json.dumps(notification) + "\n")
        self.process.stdin.flush()

    def _read_response(self, timeout: float = 10.0) -> dict:
        import select
        fd = self.process.stdout.fileno()
        ready, _, _ = select.select([fd], [], [], timeout)
        if not ready:
            return {"error": "Timeout"}
        line = self.process.stdout.readline()
        if not line:
            return {"error": "No response"}
        return json.loads(line)

    def call_tool(self, tool_name: str, arguments: dict) -> dict:
        self._send_request("tools/call", {"name": tool_name, "arguments": arguments})
        return self._read_response()


def main():
    TEST_DIR.mkdir(parents=True, exist_ok=True)
    for f in TEST_DIR.glob("*"):
        f.unlink()

    client = WinxMCPClient()

    try:
        print("Starting Winx MCP server...")
        client.start()

        # Step 1: Initialize
        print("\n=== Step 1: Initialize ===")
        result = client.call_tool("Initialize", {
            "type": "first_call",
            "any_workspace_path": str(TEST_DIR),
            "initial_files_to_read": [],
            "task_id_to_resume": "",
            "mode_name": "wcgw",
            "thread_id": THREAD_ID,
            "code_writer_config": None
        })
        print(f"Initialize result: {'OK' if 'error' not in result else result['error']}")

        # Step 2: Create file
        print("\n=== Step 2: Create file ===")
        test_file = TEST_DIR / "test.py"
        content1 = '''def hello():
    print("Hello")

def main():
    hello()
'''
        result = client.call_tool("FileWriteOrEdit", {
            "file_path": str(test_file),
            "percentage_to_change": 100,
            "text_or_search_replace_blocks": content1,
            "thread_id": THREAD_ID
        })
        print(f"Create result: {'OK' if 'error' not in result else result['error']}")

        # Step 3: Read file
        print("\n=== Step 3: Read file ===")
        result = client.call_tool("ReadFiles", {
            "file_paths": [str(test_file)]
        })
        print(f"Read result: {'OK' if 'error' not in result else result['error']}")

        # Step 4: Edit file
        print("\n=== Step 4: Edit file (first edit) ===")
        result = client.call_tool("FileWriteOrEdit", {
            "file_path": str(test_file),
            "percentage_to_change": 30,
            "text_or_search_replace_blocks": '''<<<<<<< SEARCH
def hello():
    print("Hello")
=======
def hello(name="World"):
    print(f"Hello, {name}")
>>>>>>> REPLACE''',
            "thread_id": THREAD_ID
        })
        if "error" in result:
            print(f"Edit 1 FAILED: {result['error']}")
        else:
            print(f"Edit 1 OK: {result.get('result', {}).get('content', [{}])[0].get('text', '')[:100]}")

        # Read file content
        print(f"\nFile content after edit 1:")
        print(test_file.read_text())

        # Step 5: Read file again
        print("\n=== Step 5: Read file again ===")
        result = client.call_tool("ReadFiles", {
            "file_paths": [str(test_file)]
        })
        print(f"Read result: {'OK' if 'error' not in result else result['error']}")

        # Step 6: Edit file again
        print("\n=== Step 6: Edit file (second edit) ===")
        result = client.call_tool("FileWriteOrEdit", {
            "file_path": str(test_file),
            "percentage_to_change": 30,
            "text_or_search_replace_blocks": '''<<<<<<< SEARCH
def main():
    hello()
=======
def main():
    hello("Winx")
>>>>>>> REPLACE''',
            "thread_id": THREAD_ID
        })
        if "error" in result:
            print(f"Edit 2 FAILED: {result['error']}")
        else:
            print(f"Edit 2 OK: {result.get('result', {}).get('content', [{}])[0].get('text', '')[:100]}")

        # Final content
        print(f"\nFinal file content:")
        print(test_file.read_text())

    finally:
        client.stop()


if __name__ == "__main__":
    main()
