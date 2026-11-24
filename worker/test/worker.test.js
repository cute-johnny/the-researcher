// node --test test/   (Node 22.13+; the D1 stand-in is node:sqlite running the real schema.sql)
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";

import worker, { badgeSvg, short } from "../src/index.js";
import { MAX_LOADS, parsePayload, recordError, rowHash } from "../src/validate.js";

const NOW = Date.UTC(2026, 10, 16, 12);
const record = (over = {}) => ({
  chars: 69652,
  day: "2026-11-16",
  docs: 7,
  fmt: { html: 4, md: 3, pdf: 0, txt: 0 },
  id: "7d75b3c23f2e4970",
  lib_chars: 2239282,
  lib_docs: 114,
  raw: 69655,
  src_bytes: 113432,
  tk: 2300,
  v: 1,
  ...over,
});
/** A version 2 record: `fmt` has chats too, `fmt_chars` is the loaded characters per format. */
const record2 = (over = {}) => ({
  ...record(),
  v: 2,
  fmt: { chat: 0, html: 4, md: 3, pdf: 0, txt: 0 },
  fmt_chars: { chat: 0, html: 40000, md: 29652, pdf: 0, txt: 0 }, // adds up to chars (69652)
  ...over,
});
const payload = (...loads) => ({ schema: 1, loads: loads.length ? loads : [record()] });
/** What `researcher --usage-send` sends: schema 2, every record numbered by its log line. */
const sent = (...loads) => ({ schema: 2, loads: loads.map((r, i) => ({ ...r, seq: r.seq ?? i + 1 })) });

/** The few D1 calls the Worker uses, on an in-memory SQLite with the real schema. */
function fakeD1() {
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("../schema.sql", import.meta.url), "utf8"));
  const make = (sql) => ({
    sql,
    args: [],
    bind(...a) {
      this.args = a;
      return this;
    },
    async first() {
      return db.prepare(sql).get(...this.args) ?? null;
    },
  });
  return {
    db,
    prepare: make,
    async batch(stmts) {
      return stmts.map((s) => ({ meta: { changes: Number(db.prepare(s.sql).run(...s.args).changes) } }));
    },
  };
}

const post = (body, env, headers = {}) =>
  worker.fetch(
    new Request("https://x.test/v1/usage", {
      method: "POST",
      headers: { "content-type": "application/json", ...headers },
      body: typeof body === "string" ? body : JSON.stringify(body),
    }),
    env,
  );

test("a real export record is accepted", () => {
  assert.equal(recordError(record(), NOW), null);
  const { rows, error } = parsePayload(payload(), NOW);
  assert.equal(error, undefined);
  assert.equal(rows[0].md, 3);
});

test("anything outside the whitelist is refused", () => {
  const bad = [
    record({ title: "My research" }),
    record({ path: "/home/me/x.html" }),
    record({ id: "not-an-id-at-all!" }),
    record({ id: "7D75B3C23F2E4970" }),
    record({ day: "2026-13-40" }),
    record({ day: "2025-12-31" }),
    record({ day: "2027-06-01" }),
    record({ docs: 8 }), // fmt must add up
    record({ fmt: { html: 4, md: 3, pdf: 0 } }),
    record({ fmt: { html: 4, md: 3, pdf: 0, txt: 0, other: 1 } }),
    record({ chars: -1 }),
    record({ chars: 1.5 }),
    record({ raw: "69655" }),
    record({ tk: 0 }),
    record({ tk: 1, raw: 1e9, lib_chars: 1e9 }), // would add a billion tokens per record
    record({ lib_chars: 2e9 }),
    record({ v: 2 }),
  ];
  for (const rec of bad) assert.notEqual(recordError(rec, NOW), null, JSON.stringify(rec));
  assert.ok(parsePayload({ schema: 1, loads: [], extra: 1 }, NOW).error);
  assert.ok(parsePayload({ schema: 2, loads: [record()] }, NOW).error);
  assert.ok(parsePayload({ schema: 1, loads: [] }, NOW).error);
  assert.ok(parsePayload({ schema: 1, loads: Array(MAX_LOADS + 1).fill(record()) }, NOW).error);
});

test("schema 2 needs a seq on every record, schema 1 allows none", () => {
  assert.equal(parsePayload(sent(record()), NOW).error, undefined);
  assert.equal(parsePayload(sent(record()), NOW).rows[0].seq, 1);
  assert.equal(parsePayload(payload(), NOW).rows[0].seq, null);
  assert.ok(parsePayload({ schema: 2, loads: [record()] }, NOW).error); // no seq
  assert.ok(parsePayload({ schema: 1, loads: [record({ seq: 1 })] }, NOW).error); // seq in schema 1
  for (const seq of [0, -1, 1.5, "3", null, 1_000_000_001]) {
    assert.ok(parsePayload({ schema: 2, loads: [record({ seq })] }, NOW).error, String(seq));
  }
  assert.ok(parsePayload({ schema: 3, loads: [record({ seq: 1 })] }, NOW).error);
});

test("schema 1 rows keep the hash they had before seq existed", async () => {
  const { rows } = parsePayload(payload(), NOW);
  const old = createHash("sha256")
    .update("7d75b3c23f2e4970|2026-11-16|7|0|4|3|0|113432|69655|69652|114|2239282|2300")
    .digest("hex");
  assert.equal(await rowHash(rows[0]), old);
  assert.notEqual(await rowHash(parsePayload(sent(record()), NOW).rows[0]), old);
});

test("two identical loads with their own seq are both kept; a resend is stored once", async () => {
  const env = { DB: fakeD1() };
  let res = await post(sent(record(), record()), env);
  assert.deepEqual(await res.json(), { ok: true, received: 2, new: 2 });
  res = await post(sent(record(), record()), env);
  assert.deepEqual(await res.json(), { ok: true, received: 2, new: 0 });
  res = await post(sent(record({ seq: 3 })), env);
  assert.deepEqual(await res.json(), { ok: true, received: 1, new: 1 });
  assert.equal(env.DB.db.prepare("SELECT COUNT(*) AS n FROM loads").get().n, 3);
});

test("POST stores records and the same record twice is stored once", async () => {
  const env = { DB: fakeD1() };
  let res = await post(payload(record(), record({ chars: 100, raw: 120 })), env);
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { ok: true, received: 2, new: 2 });

  res = await post(payload(record()), env);
  assert.deepEqual(await res.json(), { ok: true, received: 1, new: 0 });

  const row = env.DB.db.prepare("SELECT COUNT(*) AS n, SUM(docs) AS docs FROM loads").get();
  assert.equal(row.n, 2);
  assert.equal(row.docs, 14);
});

test("a bad record stores nothing, not even the good ones", async () => {
  const env = { DB: fakeD1() };
  const res = await post(payload(record(), record({ title: "x" })), env);
  assert.equal(res.status, 400);
  assert.match((await res.json()).error, /^record 1:/);
  assert.equal(env.DB.db.prepare("SELECT COUNT(*) AS n FROM loads").get().n, 0);
});

test("wrong method, content type, JSON and size", async () => {
  const env = { DB: fakeD1() };
  let res = await worker.fetch(new Request("https://x.test/v1/usage"), env);
  assert.equal(res.status, 405);

  res = await post(payload(), env, { "content-type": "text/plain" });
  assert.equal(res.status, 415);

  res = await post("{not json", env);
  assert.equal(res.status, 400);

  res = await post("x".repeat(300_000), env);
  assert.equal(res.status, 413);
});

test("rate limit answers 429 and stores nothing", async () => {
  const env = { DB: fakeD1(), RATE: { limit: async () => ({ success: false }) } };
  const res = await post(payload(), env);
  assert.equal(res.status, 429);
  assert.equal(env.DB.db.prepare("SELECT COUNT(*) AS n FROM loads").get().n, 0);
});

test("stats are totals only", async () => {
  const env = { DB: fakeD1() };
  await post(payload(record(), record({ id: "0123456789abcdef", chars: 10, raw: 20 })), env);
  const res = await worker.fetch(new Request("https://x.test/v1/stats"), env);
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), {
    installs: 2,
    loads: 2,
    documents: 14,
    raw_chars: 69655 + 20,
    chars: 69652 + 10,
    // per record with its own tk (2300): (3 + 10) chars squeezed, (lib_chars - chars) not loaded
    tokens_squeezed: Math.round((3 * 1000) / 2300 + (10 * 1000) / 2300),
    tokens_from_squeeze: Math.round((3 * 1000) / 2300 + (10 * 1000) / 2300),
    tokens_from_conversion: 0, // version 1 records do not say which formats their characters are
    chat_loads: 0,
    loads_with_formats: 0,
    tokens_not_loaded: Math.round(((2239282 - 69652) * 1000) / 2300 + ((2239282 - 10) * 1000) / 2300),
  });
});

test("badges: shields.io endpoint JSON with the two token totals", async () => {
  const env = { DB: fakeD1() };
  await post(sent(record({ raw: 69652 + 2300, lib_chars: 69652 }), record({ lib_chars: 69652 + 2_300_000 })), env);
  let res = await worker.fetch(new Request("https://x.test/v1/badge/squeezed"), env);
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("cache-control"), "public, max-age=300"); // shields.io's minimum
  assert.deepEqual(await res.json(), {
    schemaVersion: 1, label: "tokens squeezed", message: "1k", color: "brightgreen", cacheSeconds: 300,
  });
  res = await worker.fetch(new Request("https://x.test/v1/badge/not-loaded"), env);
  assert.equal((await res.json()).message, "1M");
  for (const p of ["/v1/badge/", "/v1/badge/toString", "/v1/badge/__proto__", "/v1/badge/squeezed/x"]) {
    assert.equal((await worker.fetch(new Request("https://x.test" + p), env)).status, 404, p);
  }
  res = await worker.fetch(new Request("https://x.test/v1/badge/squeezed", { method: "POST" }), env);
  assert.equal(res.status, 405);
});

test("badges: SVG images drawn by the Worker itself", async () => {
  const env = { DB: fakeD1() };
  await post(sent(record({ raw: 69652 + 2300, lib_chars: 69652 }), record({ lib_chars: 69652 + 2_300_000 })), env);
  let res = await worker.fetch(new Request("https://x.test/v1/badge/squeezed.svg"), env);
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "image/svg+xml; charset=utf-8");
  assert.equal(res.headers.get("cache-control"), "public, max-age=300");
  let svg = await res.text();
  assert.match(svg, /^<svg xmlns="http:\/\/www.w3.org\/2000\/svg" width="\d+" height="20"/);
  assert.match(svg, /<title>tokens squeezed: 1k<\/title>/);
  assert.match(svg, /fill="#4c1"/);
  svg = await (await worker.fetch(new Request("https://x.test/v1/badge/not-loaded.svg"), env)).text();
  assert.match(svg, /<title>tokens not loaded: 1M<\/title>/);
  for (const p of ["/v1/badge/.svg", "/v1/badge/toString.svg", "/v1/badge/squeezed.svg/x", "/v1/badge/squeezed.png"]) {
    assert.equal((await worker.fetch(new Request("https://x.test" + p), env)).status, 404, p);
  }
  res = await worker.fetch(new Request("https://x.test/v1/badge/squeezed.svg", { method: "POST" }), env);
  assert.equal(res.status, 405);
  assert.match(badgeSvg("a<b", "&", "#fff"), /<title>a&lt;b: &amp;<\/title>/);
});

test("short numbers for badges", () => {
  assert.deepEqual([0, 999, 1000, 1234, 99_960, 123_456, 2_712_345, 3e9].map(short),
    ["0", "999", "1k", "1.2k", "100k", "123k", "2.7M", "3B"]);
});

test("nothing else is served", async () => {
  const env = { DB: fakeD1() };
  assert.equal((await worker.fetch(new Request("https://x.test/"), env)).status, 200);
  assert.equal((await worker.fetch(new Request("https://x.test/admin"), env)).status, 404);
});

test("version 2 records: chats, characters per format, and they must add up", () => {
  assert.equal(recordError(record2(), NOW), null);
  assert.equal(recordError(record2({ fmt: { chat: 7, html: 0, md: 0, pdf: 0, txt: 0 }, fmt_chars: { chat: 69652, html: 0, md: 0, pdf: 0, txt: 0 } }), NOW), null);
  const bad = [
    record2({ fmt_chars: undefined }), // v2 needs fmt_chars
    record2({ fmt_chars: { chat: 0, html: 1, md: 1, pdf: 0, txt: 0 } }), // does not add up to chars
    record2({ fmt_chars: { html: 40000, md: 29652, pdf: 0, txt: 0 } }), // chat missing
    record2({ fmt: { html: 4, md: 3, pdf: 0, txt: 0 } }), // v1 fmt in a v2 record
    record2({ fmt: { chat: 1, html: 4, md: 3, pdf: 0, txt: 0 } }), // does not add up to docs
    record2({ fmt_chars: { chat: 0, html: -1, md: 69653, pdf: 0, txt: 0 } }),
    record({ fmt_chars: { chat: 0, html: 40000, md: 29652, pdf: 0, txt: 0 } }), // v1 has none
    record2({ v: 3 }),
    record2({ title: "x" }),
  ];
  for (const rec of bad) assert.notEqual(recordError(rec, NOW), null, JSON.stringify(rec));
  const { rows } = parsePayload(sent(record2()), NOW);
  assert.deepEqual([rows[0].chat, rows[0].html_chars, rows[0].md_chars, rows[0].pdf_chars], [0, 40000, 29652, 0]);
  assert.equal(parsePayload(payload(record2()), NOW).error, undefined); // schema 1 file, v2 record
});

test("version 1 hashes are unchanged; version 2 adds its numbers to the hash", async () => {
  const old = createHash("sha256")
    .update("7d75b3c23f2e4970|2026-11-16|7|0|4|3|0|113432|69655|69652|114|2239282|2300")
    .digest("hex");
  assert.equal(await rowHash(parsePayload(payload(), NOW).rows[0]), old);
  const a = await rowHash(parsePayload(payload(record2()), NOW).rows[0]);
  const b = await rowHash(parsePayload(payload(record2({ fmt_chars: { chat: 0, html: 39999, md: 29653, pdf: 0, txt: 0 } })), NOW).rows[0]);
  assert.notEqual(a, old);
  assert.notEqual(a, b);
});

test("tokens squeezed = the squeeze + reading text instead of the HTML/PDF original", async () => {
  const env = { DB: fakeD1() };
  // 2300 characters per 1000 tokens. One load: 4600 raw -> 2300 loaded (squeeze 2300 chars),
  // 1000 html chars (saves 0.73 each) and 500 pdf chars (saves 1.2 each) and 800 md, no saving.
  const rec = record2({
    raw: 4600, chars: 2300, tk: 2300,
    docs: 6, fmt: { chat: 0, html: 2, md: 2, pdf: 2, txt: 0 },
    fmt_chars: { chat: 0, html: 1000, md: 800, pdf: 500, txt: 0 },
  });
  await post(sent(rec, record()), env); // plus one version 1 load: 3 chars squeezed, no conversion part
  const stats = await (await worker.fetch(new Request("https://x.test/v1/stats"), env)).json();
  const squeeze = (2300 * 1000) / 2300 + (3 * 1000) / 2300;
  const convert = ((1000 * 0.73 + 500 * 1.2) * 1000) / 2300;
  assert.equal(stats.tokens_from_squeeze, Math.round(squeeze));
  assert.equal(stats.tokens_from_conversion, Math.round(convert));
  assert.equal(stats.tokens_squeezed, Math.round(squeeze + convert));
  assert.equal(stats.loads_with_formats, 1);
  const badge = await (await worker.fetch(new Request("https://x.test/v1/badge/squeezed"), env)).json();
  assert.equal(badge.message, short(squeeze + convert));
});

test("chat loads are counted as loads and have no conversion saving", async () => {
  const env = { DB: fakeD1() };
  const chat = record2({
    docs: 2, fmt: { chat: 2, html: 0, md: 0, pdf: 0, txt: 0 },
    fmt_chars: { chat: 69652, html: 0, md: 0, pdf: 0, txt: 0 }, raw: 69652,
  });
  await post(sent(chat), env);
  const stats = await (await worker.fetch(new Request("https://x.test/v1/stats"), env)).json();
  assert.equal(stats.chat_loads, 1);
  assert.equal(stats.tokens_from_conversion, 0);
  assert.equal(stats.tokens_from_squeeze, 0);
  assert.equal(stats.documents, 2);
});

test("the migration brings an old table to the new shape", () => {
  const db = new DatabaseSync(":memory:");
  // the table as it was before record version 2
  db.exec(`CREATE TABLE loads (h TEXT PRIMARY KEY, id TEXT NOT NULL, day TEXT NOT NULL, docs INTEGER NOT NULL,
    pdf INTEGER NOT NULL, html INTEGER NOT NULL, md INTEGER NOT NULL, txt INTEGER NOT NULL, src_bytes INTEGER NOT NULL,
    raw INTEGER NOT NULL, chars INTEGER NOT NULL, lib_docs INTEGER NOT NULL, lib_chars INTEGER NOT NULL, tk INTEGER NOT NULL)`);
  db.exec("INSERT INTO loads VALUES ('h','7d75b3c23f2e4970','2026-11-16',1,0,1,0,0,1,1,1,1,1,2300)");
  db.exec(readFileSync(new URL("../migrations/0002_formats.sql", import.meta.url), "utf8"));
  const row = db.prepare("SELECT chat, pdf_chars, html_chars, chat_chars FROM loads").get();
  assert.deepEqual({ ...row }, { chat: 0, pdf_chars: null, html_chars: null, chat_chars: null });
  // the migrated table has the same columns as a fresh one
  const fresh = new DatabaseSync(":memory:");
  fresh.exec(readFileSync(new URL("../schema.sql", import.meta.url), "utf8"));
  const cols = (d) => d.prepare("PRAGMA table_info(loads)").all().map((c) => c.name).sort();
  assert.deepEqual(cols(db), cols(fresh));
});

test("badge numbers that round up move to the next unit", () => {
  assert.equal(short(999), "999");
  assert.equal(short(999_499), "999k");
  assert.equal(short(999_500), "1M");
  assert.equal(short(999_500_000), "1B");
  assert.equal(short(2_712_345), "2.7M");
});
