"""Zest plugin SDK for Python (protocol api 1, see PROTOCOL.md next to this folder).

    from zest_plugin import Plugin
    plugin = Plugin()

    @plugin.method("library.sync")
    def sync(params, ctx):
        plugin.games([{"platform_id": "1", "name": "A Game"}])
        return {"count": 1}

    plugin.run()

Requests from Zest each run in their own thread, so a handler may ask Zest for things (secrets, known ids) while it works. A handler gets `(params, ctx)`;
`ctx.cancelled()` becomes true when Zest cancels the request, and a handler that loops should check it. An exception becomes an error message shown to
the user: raise `PluginError("text for the user")` for expected failures. `print()` goes to stderr, never into the protocol.
"""
import itertools
import json
import sys
import threading
import traceback

API = 1


class PluginError(Exception):
    def __init__(self, message, code=-32000):
        super().__init__(message)
        self.message, self.code = message, code


class _Ctx:
    def __init__(self, plugin, rid):
        self.id, self._plugin = rid, plugin

    def cancelled(self):
        return self.id in self._plugin._cancelled


class _Secrets:
    def __init__(self, plugin):
        self._p = plugin

    def get(self, key):
        return self._p.request("secrets.get", {"key": key}).get("value")

    def set(self, key, value):
        self._p.request("secrets.set", {"key": key, "value": value})

    def delete(self, key):
        self._p.request("secrets.delete", {"key": key})


class Plugin:
    def __init__(self, name=None, capabilities=None):
        self._handlers = {}
        self._pending = {}
        self._cancelled = set()
        self._ids = itertools.count(1)
        self._write_lock = threading.Lock()
        self._out = sys.stdout
        self.secrets = _Secrets(self)
        self.name, self.capabilities = name, capabilities

    # ---- registering handlers
    def method(self, name):
        def deco(fn):
            self._handlers[name] = fn
            return fn
        return deco

    # ---- talking to Zest
    def _send(self, obj):
        line = json.dumps(obj, separators=(",", ":"), ensure_ascii=False)
        with self._write_lock:
            self._out.write(line + "\n")
            self._out.flush()

    def request(self, method, params=None, timeout=60):
        rid = next(self._ids)
        ev = threading.Event()
        slot = {"event": ev}
        self._pending[rid] = slot
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        if not ev.wait(timeout):
            self._pending.pop(rid, None)
            raise PluginError("Zest did not answer " + method)
        if "error" in slot:
            raise PluginError(slot["error"].get("message", "error"), slot["error"].get("code", -32000))
        return slot.get("result")

    def notify(self, method, params=None):
        self._send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def games(self, records):
        """Hand Zest a batch of game records (see PROTOCOL.md). Call as often as you like; batches of 50 to 200 are good."""
        self.notify("library.games", {"games": list(records)})

    def progress(self, done, total=None, message=None):
        self.notify("progress", {"done": done, "total": total, "message": message})

    def log(self, message, level="info"):
        print(message, file=sys.stderr, flush=True)

    def known_ids(self):
        """The platform_ids Zest already has for this plugin's games (to skip slow per-game lookups)."""
        return set(self.request("core.known_ids", {}).get("ids", []))

    # ---- the loop
    def _handle(self, rid, method, params):
        ctx = _Ctx(self, rid)
        try:
            fn = self._handlers.get(method)
            if fn is None:
                self._send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "method not found: " + method}})
                return
            result = fn(params or {}, ctx)
            self._send({"jsonrpc": "2.0", "id": rid, "result": result})
        except PluginError as e:
            self._send({"jsonrpc": "2.0", "id": rid, "error": {"code": e.code, "message": e.message}})
        except Exception as e:  # a bug in the plugin: say so, keep running
            traceback.print_exc(file=sys.stderr)
            self._send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32603, "message": "plugin error: %s" % e}})
        finally:
            self._cancelled.discard(rid)

    def run(self):
        # anything the plugin prints must not corrupt the protocol
        sys.stdout = sys.stderr
        if "describe" not in self._handlers:
            @self.method("describe")
            def _describe(params, ctx):
                d = {"api": API}
                if self.name:
                    d["name"] = self.name
                if self.capabilities:
                    d["capabilities"] = self.capabilities
                return d
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            method = msg.get("method")
            if method:
                if "id" in msg:
                    threading.Thread(target=self._handle, args=(msg["id"], method, msg.get("params")), daemon=True).start()
                elif method == "shutdown":
                    break
                elif method == "cancel":
                    self._cancelled.add((msg.get("params") or {}).get("id"))
            elif "id" in msg:
                slot = self._pending.pop(msg["id"], None)
                if slot is not None:
                    if "error" in msg:
                        slot["error"] = msg["error"]
                    else:
                        slot["result"] = msg.get("result")
                    slot["event"].set()
