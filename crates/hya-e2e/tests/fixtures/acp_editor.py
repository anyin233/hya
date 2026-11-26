"""Drive two real ACP stdio sessions against the Track P backend and FakeLlm."""
import asyncio
import json
import os
from pathlib import Path
import sys


def mcp_peer():
    name = os.environ["FIXTURE_TOOL"]
    Path(name + ".pid").write_text(str(os.getpid()))
    for line in sys.stdin:
        req = json.loads(line)
        if "id" not in req:
            continue
        method = req["method"]
        if method == "initialize":
            result = {"protocolVersion": req["params"]["protocolVersion"],
                      "capabilities": {"tools": {}}, "serverInfo": {"name": name, "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": name, "description": name,
                                 "inputSchema": {"type": "object", "properties": {}}}]}
        elif method == "tools/call":
            assert req["params"]["name"] == name
            Path(name + ".called").touch()
            result = {"content": [{"type": "text", "text": name}]}
        elif method == "resources/list":
            result = {"resources": []}
        else:
            raise ValueError(method)
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)


class Editor:
    def __init__(self, proc):
        self.proc = proc
        self.seq = 0

    def send(self, **frame):
        self.proc.stdin.write((json.dumps(dict(jsonrpc="2.0", **frame)) + "\n").encode())

    def request(self, method, **params):
        self.seq += 1
        self.send(id=self.seq, method=method, params=params)
        return self.seq

    async def read(self):
        line = await asyncio.wait_for(self.proc.stdout.readline(), 8)
        assert line, "unexpected ACP EOF"
        frame = json.loads(line)
        if os.environ.get("HYA_ACP_TEST_TRACE"):
            print("ACP", frame, file=sys.stderr, flush=True)
        return frame

    async def response(self, ident):
        frame = await self.read()
        assert frame.get("id") == ident, frame
        return frame

    async def new(self, cwd, peers):
        req = self.request("session/new", cwd=str(cwd), mcpServers=peers)
        return await self.response(req)

    def prompt(self, session, text):
        return self.request("session/prompt", sessionId=session,
                            prompt=[{"type": "text", "text": text},
                                    {"type": "resource_link", "name": "fixture", "uri": "file:///fixture"},
                                    {"type": "text", "text": "after-link"}])

    async def turn(self, session, action, expected, other=None):
        req = self.prompt(session, action)
        statuses = {}
        chunks = []
        asked = False
        while True:
            frame = await self.read()
            if frame.get("method") == "session/request_permission":
                assert frame["params"]["sessionId"] == session
                call_id = frame["params"]["toolCall"]["toolCallId"]
                assert call_id in statuses, (frame, statuses)
                asked = True
                if action == "eof":
                    self.proc.stdin.close()
                    return
                if action == "cancel":
                    busy = self.prompt(session, "busy")
                    assert "busy" in (await self.response(busy))["error"]["message"].lower()
                    if other:
                        await other()
                    for _ in range(2):
                        self.send(method="session/cancel", params={"sessionId": session})
                else:
                    self.send(id=frame["id"], result={"outcome": {"outcome": "selected", "optionId": action}})
            elif frame.get("method") == "$/cancel_request":
                assert asked and action == "cancel", frame
                self.send(id=frame["params"]["requestId"], result={"outcome": {"outcome": "cancelled"}})
            elif frame.get("method") == "session/update":
                assert frame["params"]["sessionId"] == session
                update = frame["params"]["update"]
                if update["sessionUpdate"] == "agent_message_chunk":
                    chunks.append(update["content"]["text"])
                elif update["sessionUpdate"] in ("tool_call", "tool_call_update"):
                    statuses[update["toolCallId"]] = update.get("status")
            else:
                assert frame.get("id") == req, frame
                reason = "cancelled" if action == "cancel" else "end_turn"
                assert frame.get("result", {}).get("stopReason") == reason, frame
                assert asked == (action in ("allow", "deny", "cancel")), (action, statuses)
                if expected:
                    assert "".join(chunks) == expected, chunks
                if action in ("allow", "deny"):
                    assert set(statuses.values()) == {"completed" if action == "allow" else "failed"}, statuses
                return


def peer(tool):
    return {"name": "peer", "command": sys.executable, "args": [str(Path(__file__).resolve()), "--mcp"],
            "env": [{"name": "FIXTURE_TOOL", "value": tool}]}


async def launch(binary, db):
    proc = await asyncio.create_subprocess_exec(binary, "--db", db, "acp",
                                                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE)
    editor = Editor(proc)
    req = editor.request("initialize", protocolVersion=1, clientCapabilities={})
    result = (await editor.response(req))["result"]
    assert result["protocolVersion"] == 1
    assert result["agentCapabilities"]["sessionCapabilities"]["close"] == {}, result
    return editor


async def exited(pidfile):
    pid = int(pidfile.read_text())
    for _ in range(100):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        await asyncio.sleep(0.01)
    raise AssertionError(f"MCP peer {pid} survived its session")


async def editor_main(binary, db):
    a = await launch(binary, db)
    b = await launch(binary, db)
    try:
        cwd = Path.cwd()
        other_dir = cwd / "second"
        other_dir.mkdir()
        assert "error" in await a.new(Path("relative"), [])
        assert "error" in await a.new(cwd, [dict(peer("unused"), name="bad__namespace")])
        invalid = dict(peer("invalid"), name="bad", command="/missing/acp-fixture")
        assert "error" in await a.new(cwd, [peer("partial"), invalid])
        first = (await a.new(cwd, [peer("alpha")]))["result"]["sessionId"]
        second = (await a.new(other_dir, [peer("beta")]))["result"]["sessionId"]
        survivor = (await b.new(cwd, []))["result"]["sessionId"]
        assert "error" in await b.response(b.prompt(first, "foreign session"))
        await a.turn(first, "allow", "ALLOWED")
        await a.turn(second, "deny", "DENIED")
        assert (cwd / "alpha.called").exists() and not (other_dir / "beta.called").exists()
        await a.turn(first, "cancel", None, other=lambda: a.turn(second, "parallel", "PARALLEL"))
        assert "result" in await a.response(a.request("session/close", sessionId=second))
        await exited(other_dir / "beta.pid")
        assert "error" in await a.response(a.prompt(second, "closed"))
        await a.turn(first, "resume", "RESUMED")
        await a.turn(first, "eof", None)
        assert await asyncio.wait_for(a.proc.wait(), 6) == 0
        await b.turn(survivor, "survive", "SURVIVED")
        req = b.prompt(survivor, "credential error")
        while True:
            frame = await b.read()
            if frame.get("id") == req:
                assert frame["error"]["code"] == -32000, frame
                assert "hya login" in frame["error"]["data"]["hint"]
                break
            assert frame.get("method") == "session/update", frame
        b.proc.stdin.close()
        assert await asyncio.wait_for(b.proc.wait(), 6) == 0
        for pidfile in [cwd / "partial.pid", cwd / "alpha.pid"]:
            await exited(pidfile)
    finally:
        for editor in (a, b):
            if editor.proc.returncode is None:
                editor.proc.kill()
                await editor.proc.wait()


if __name__ == "__main__":
    if sys.argv[1] == "--mcp":
        mcp_peer()
    else:
        asyncio.run(editor_main(*sys.argv[1:]))
