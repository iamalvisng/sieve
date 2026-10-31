# Times warm MCP calls. Run it through scripts/bench-mcp.sh.
# usage: bench-mcp.py <sieve-binary> <repo> <symbol> <query>
import json, subprocess, sys, time, statistics

binary, repo, symbol, query = sys.argv[1:5]
CASES = [
    ("query", "sieve_query", {"query": query}),
    ("find literal", "sieve_find", {"pattern": symbol, "fixed": True}),
    ("find regex", "sieve_find", {"pattern": symbol + r"\w*"}),
    ("callers", "sieve_callers", {"symbol": symbol}),
]
p = subprocess.Popen([binary, "mcp"], cwd=repo, stdin=subprocess.PIPE,
                     stdout=subprocess.PIPE, text=True)
n = 0


def rpc(method, params=None, note=False):
    global n
    m = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        m["params"] = params
    if not note:
        n += 1
        m["id"] = n
    p.stdin.write(json.dumps(m) + "\n")
    p.stdin.flush()
    if note:
        return None
    while True:
        r = json.loads(p.stdout.readline())
        if r.get("id") == n:
            return r


rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "bench", "version": "1"}})
rpc("notifications/initialized", note=True)
for name, tool, args in CASES:
    def call():
        t = time.perf_counter()
        rpc("tools/call", {"name": tool, "arguments": args})
        return (time.perf_counter() - t) * 1000

    call()  # warm-up
    ts = [call() for _ in range(5)]
    print(f"{name}: median {statistics.median(ts):.1f} ms  runs {[round(x, 1) for x in ts]}")
p.stdin.close()
p.wait()
