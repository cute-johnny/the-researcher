# Usage receiver (Cloudflare Worker)

The receiving end for the researcher's **opt-in, anonymous** usage counts. The app only
sends here after the user said yes (`researcher --usage-share on`); see "Sharing" in the
main README. No dependencies: plain JavaScript, a D1 database, nothing else.

## What it accepts

`POST /v1/usage` with `Content-Type: application/json`. Two shapes:

- **schema 2**, what `researcher --usage-send` sends: every record also has `seq`, its line
  number in the user's log (`researcher --usage-send --dry-run` prints the exact body);
- **schema 1**, the shape of the file `researcher --usage-export` writes (no `seq`). Don't post
  that file from an install that also shares automatically: its rows have no `seq`, so every
  load would be counted a second time.

```json
{"schema": 1, "loads": [
  {"v": 1, "day": "2026-11-16", "id": "7d75b3c23f2e4970", "docs": 7,
   "fmt": {"html": 4, "md": 3, "pdf": 0, "txt": 0},
   "src_bytes": 113432, "raw": 69655, "chars": 69652,
   "lib_docs": 114, "lib_chars": 2239282, "tk": 2400}
]}
```

Records come in two versions (`v`): 1 has counts per format (pdf, html, md, txt); 2 also has
`chat` (earlier Claude Code chats loaded as context) in `fmt`, and `fmt_chars`, the loaded
characters per format, which must add up to `chars`. Both are accepted; old rows keep NULL
characters per format.

Validation is a strict whitelist (`src/validate.js`): any other field, any string that
is not a 16-hex id or a plausible date, any out-of-range number, or a `fmt` that does
not add up to `docs` rejects the **whole** request and stores nothing. Free text
(titles, paths, names) cannot get in. Limits: 200 records and 256 KB per request.

A record that is sent twice is stored once (primary key = hash of its numbers and, in
schema 2, its `seq`), so the app can resend its queue safely, while two identical loads on
one day (different `seq`) are two rows. Schema 1 rows hash as they always did.

`GET /v1/stats` returns public totals only: installs, loads, documents, characters, chat loads,
and the token estimates (per record, characters / `tk` × 1000): `tokens_not_loaded`, and
`tokens_squeezed` = `tokens_from_squeeze` (raw minus loaded characters) + `tokens_from_conversion`
(0.73 per loaded HTML character and 1.2 per PDF character: reading text instead of the original,
from the ratios in the main README; only records that carry `fmt_chars` add to it, and
`loads_with_formats` says how many do).
`GET /v1/badge/squeezed.svg` and `GET /v1/badge/not-loaded.svg` draw the same two numbers as
badge images, for the README badges. `GET /v1/badge/squeezed` and `GET /v1/badge/not-loaded`
give them in the shields.io endpoint format
(`https://img.shields.io/endpoint?url=<worker>/v1/badge/squeezed`); the README no longer uses
those, because GitHub's image proxy timed out waiting on shields.io.

## What it keeps and does not keep

Kept: the random install id, the day, document counts per format, character sums.

Not kept: IP address, headers, user agent, titles, paths, GitHub login. The code never
reads them (the optional rate limiter uses the IP only as its key), and
`observability` is off in `wrangler.toml`, so Cloudflare keeps no request logs for this
Worker. Cloudflare itself still sees connections as any host does.

## Upgrade an existing database

A database made before record version 2 needs one migration (new nullable columns; old rows keep
their data), then redeploy:

```sh
npx wrangler d1 execute researcher-usage --remote --file migrations/0002_formats.sql
npx wrangler deploy
```

Run the migration once: a second run fails on the first `ALTER` and changes nothing.

## Deploy

```sh
cd worker
npx wrangler login
npx wrangler d1 create researcher-usage      # prints database_id -> put it in wrangler.toml
npx wrangler d1 execute researcher-usage --remote --file schema.sql
npx wrangler deploy                          # prints https://the-researcher-usage.<you>.workers.dev
```

To keep your account's ids out of git, copy `wrangler.toml` to a file git ignores (for
example `wrangler.local.toml`, listed in `.git/info/exclude`), put the id there, and add
`--config wrangler.local.toml` to every wrangler command. An app that should send to your
own copy gets `[usage] url = "https://.../v1/usage"` in its config.

Read the numbers yourself:

```sh
npx wrangler d1 execute researcher-usage --remote --command \
  "SELECT day, COUNT(DISTINCT id) AS installs, COUNT(*) AS loads, SUM(chars) AS chars FROM loads GROUP BY day ORDER BY day DESC LIMIT 14"
```

Delete one install's data on request: `DELETE FROM loads WHERE id = '<their id>'`.

Optional: switch on the rate limit block in `wrangler.toml` (not tested here; check
Cloudflare's current "Rate Limiting" docs for the syntax first).

## Test

Node 22.13 or newer (the D1 stand-in is `node:sqlite` running the real `schema.sql`):

```sh
cd worker
npm test
```
