# Zest plugin protocol, api 1

A Zest plugin is a program Zest starts when it needs it and stops when it is idle. Zest and the plugin talk over the plugin's **stdin and stdout**, one JSON
object per line (UTF-8, no line breaks inside a message), in the style of JSON-RPC 2.0. **stderr** is the plugin's log: every line ends up in Zest's log.
A plugin never touches Zest's database or window. It returns data; Zest writes it (assigns ids, skips games the user removed, dedupes, fetches art).

## plugin.json

```json
{
  "id": "toy", "name": "Toy Store", "version": "2026.11.1", "platform": "toy",
  "zest": {
    "api": 1,
    "run": ["python3", "plugin.py"],
    "capabilities": ["auth", "sync", "rescrape", "launch"],
    "store_url": "https://toy.example/game/{slug}",
    "store_label": "View on Toy Store"
  }
}
```
`id` and `platform` are letters, digits and underscores. `run` is started with the plugin's folder as its working directory. `api` is the protocol version the
plugin speaks (Zest refuses a higher one than it knows). A PlayDate (Python) plugin can carry the same file: PlayDate ignores the `zest` key.
Capabilities: `auth`, `sync`, `rescrape`, `description`, `art`, `store_page_metadata`, `launch`, `uninstall`, `installed_scan`, `junk_scan`.

## Messages

Request `{"jsonrpc":"2.0","id":7,"method":"game.rescrape","params":{...}}`, answered by `{"jsonrpc":"2.0","id":7,"result":{...}}` or
`{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"text shown to the user"}}`. A notification has no `id` and gets no answer. Either side may send
requests while another is pending (a plugin asks `secrets.get` while Zest waits for `library.sync`). Every request Zest sends has a timeout (default 60 s;
`library.sync` and installs have none but can be cancelled).

### Zest to the plugin
| method | params | result |
|---|---|---|
| `describe` | `{core_api}` | `{api, name?, capabilities?}` |
| `auth.status` | | `{connected: bool, username?: str}` |
| `auth.begin` | | `{kind: "open_url_paste", url, instructions: [str], placeholder?}` (Zest opens the browser; the user pastes a code, URL or cookie) |
| `auth.complete` | `{value: str}` | `{connected, username?}` |
| `auth.disconnect` | | `{}` |
| `library.sync` | `{}` | `{count}`; sends `library.games` notifications first |
| `game.rescrape` | `{game}` | `{meta: {column: value}}` or `null` |
| `game.description` | `{game}` | `{text}` or `null` |
| `game.art_urls` | `{game}` | `{vertical?, horizontal?, icon?}` (addresses or local paths) |
| `game.store_page_metadata` | `{slug}` | `{genres?, categories?, tags?, developers?, publishers?, release_date?}` |
| `game.launch` | `{game}` | `{action, record?: bool}` |
| `game.uninstall` | `{game}` | `{action}` |
| `installed.paths` | | `{paths: [str]}` folders Zest watches |
| `installed.scan` | | `{installed: [{platform_id, install_path?}]}` |
| `library.junk_scan` | | `{junk: [{platform_id, name, reason}]}` |
| `shutdown` | | (notification: exit) |
| `cancel` | `{id}` | (notification: stop the request with that id) |

`game` = `{appid, name, platform_id, slug, appname, installed, install_path}`.
`action` = `{"type":"open_url","url":...}` | `{"type":"spawn","program":...,"args":[...]}` | `{"type":"message","text":...}`. Zest carries it out.

### Plugin to Zest
| method | params | result |
|---|---|---|
| `secrets.get` | `{key}` | `{value: str\|null}` |
| `secrets.set` | `{key, value}` | `{}` |
| `secrets.delete` | `{key}` | `{}` |
| `core.known_ids` | `{}` | `{ids: [str]}` the `platform_id`s already in the library for this plugin's platform |
| `library.games` (notification) | `{games: [record]}` | |
| `progress` (notification) | `{done, total?, message?}` | |
| `log` (notification) | `{level, message}` | |

`record` = `{platform_id, name, slug?, appname?, namespace?, playtime_forever? (minutes), last_played? (unix), date_added? (unix), release_date? (unix),
developers?, publishers?, genres?, categories?, tags? (each a list of strings), description?, installed?, install_path?}`. Only `platform_id` and `name` are
required. Zest keeps what the user or a scrape set: for a game already in the library a record refreshes playtime, last played, install state and links, and
fills blank metadata.

`game.rescrape` `meta` may set only these columns (others are rejected): `name`, `developers`, `publishers`, `genres`, `categories`, `tags`, `release_date`,
`short_description`, `review_score`, `review_percentage`, `weighted_percentage`, `total_reviews`, `positive_reviews`, `total_achievements`,
`unlocked_achievements`, `playtime_forever`, `last_played`, `is_free`, `metacritic_score`, `platform_slug`, `platform_appname`, `platform_ns`, `installed`,
`install_path`, `meta_fetched`, `cheevos_fetched`.

## Secrets
`secrets.*` store values in Zest's `config.json` under the plugin's id, the same place PlayDate plugins keep their tokens, so a login carries over.
Keep tokens there, never in the plugin's folder (a plugin update replaces the folder).

## Rules a plugin can rely on
* Zest starts it lazily, sends `describe` first, and sends `shutdown` after about 30 s without work. State must live in `secrets`, not in memory.
* A plugin that exits or hangs does not harm Zest: the request fails with an error, and the next request starts it again.
* Anything on stdout that is not a JSON message is ignored (and logged), so a stray `print` does not break the protocol; use stderr for logging.
