"""A toy store: three games, a fake sign-in, everything the protocol can do in a few lines. Copy this folder to start a plugin."""
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "python"))
from zest_plugin import Plugin, PluginError  # noqa: E402

plugin = Plugin(name="Toy Store", capabilities=["auth", "sync", "rescrape", "description", "launch", "installed_scan"])

GAMES = [
    {"platform_id": "t1", "name": "Toy Tanks", "slug": "toy-tanks", "playtime_forever": 125, "genres": ["Action", "Arcade"], "tags": ["Tanks", "Co-op"], "developers": ["Toybox"], "release_date": 1262304000},
    {"platform_id": "t2", "name": "Block Builder", "slug": "block-builder", "playtime_forever": 0, "genres": ["Sandbox"], "installed": True, "install_path": "/games/block-builder"},
    {"platform_id": 3, "name": "Puzzle Pals", "slug": "puzzle-pals", "last_played": 1700000000},
]


def need_login():
    if not plugin.secrets.get("token"):
        raise PluginError("Sign in to Toy Store first.")


@plugin.method("auth.status")
def auth_status(params, ctx):
    return {"connected": bool(plugin.secrets.get("token")), "username": plugin.secrets.get("username")}


@plugin.method("auth.begin")
def auth_begin(params, ctx):
    return {"kind": "open_url_paste", "url": "https://toy.example/login", "instructions": ["Sign in.", "Paste the code shown."], "placeholder": "code"}


@plugin.method("auth.complete")
def auth_complete(params, ctx):
    code = (params.get("value") or "").strip()
    if code != "letmein":
        raise PluginError("That code is not right.")
    plugin.secrets.set("token", "tok-" + code)
    plugin.secrets.set("username", "toyfan")
    return {"connected": True, "username": "toyfan"}


@plugin.method("auth.disconnect")
def auth_disconnect(params, ctx):
    plugin.secrets.delete("token")
    plugin.secrets.delete("username")
    return {}


@plugin.method("library.sync")
def sync(params, ctx):
    need_login()
    known = plugin.known_ids()
    slow = plugin.secrets.get("slow") == "1"
    sent = 0
    for i, g in enumerate(GAMES):
        if ctx.cancelled():
            break
        if slow:
            for _ in range(100):  # a slow store: about 10 s, stoppable
                if ctx.cancelled():
                    break
                time.sleep(0.1)
        plugin.progress(i + 1, len(GAMES), "game %d of %d%s" % (i + 1, len(GAMES), " (already known)" if str(g["platform_id"]) in known else ""))
        plugin.games([g])
        sent += 1
    return {"count": sent}


@plugin.method("game.rescrape")
def rescrape(params, ctx):
    g = params["game"]
    if g["platform_id"] == "t1":
        return {"meta": {"developers": ["Toybox", "Tank Works"], "review_score": "Very Positive", "review_percentage": 91, "total_reviews": 120, "evil_column": "DROP TABLE games", "installed": True}}
    return None


@plugin.method("game.description")
def description(params, ctx):
    return {"text": "A toy game called %s." % params["game"]["name"]}


@plugin.method("game.launch")
def launch(params, ctx):
    g = params["game"]
    if g.get("installed"):
        return {"action": {"type": "spawn", "program": g["install_path"] + "/run", "args": ["--toy"]}, "record": True}
    return {"action": {"type": "open_url", "url": "toystore://install/" + g["platform_id"]}, "record": False}


@plugin.method("installed.paths")
def installed_paths(params, ctx):
    return {"paths": ["/games"]}


@plugin.method("installed.scan")
def installed_scan(params, ctx):
    return {"installed": [{"platform_id": "t2", "install_path": "/games/block-builder"}]}


# --- helpers for Zest's own tests of the host (a real plugin has no use for these)
@plugin.method("debug.sleep")
def debug_sleep(params, ctx):
    time.sleep(float(params.get("seconds", 1)))
    return {}


@plugin.method("debug.crash")
def debug_crash(params, ctx):
    os._exit(3)


@plugin.method("debug.noise")
def debug_noise(params, ctx):
    print("this print must not break the protocol")
    sys.__stdout__.write("neither must a raw line that is not JSON\n")
    sys.__stdout__.flush()
    return {"ok": True}


plugin.run()
