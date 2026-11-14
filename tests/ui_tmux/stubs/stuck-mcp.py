# A stdio MCP server that answers the handshake and then never leaves: it ignores SIGTERM and stdin
# closing, so the run's wind-down waits on it (L4 45: the window a second SIGTERM must cut short).
import json, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if "id" not in msg:
        continue
    if msg.get("method") == "initialize":
        res = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
               "capabilities": {"tools": {}}, "serverInfo": {"name": "stuck", "version": "1"}}
    elif msg.get("method") == "tools/list":
        res = {"tools": []}
    else:
        res = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}) + "\n")
    sys.stdout.flush()
while True:
    time.sleep(1)
