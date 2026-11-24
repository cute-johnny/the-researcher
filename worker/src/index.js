// The researcher's opt-in, anonymous usage receiver (Cloudflare Worker + D1).
//
//   POST /v1/usage   body = what `researcher --usage-send` sends (schema 2), or the file
//                    `researcher --usage-export` writes (schema 1)
//   GET  /v1/stats   public totals: installs, loads, characters, estimated tokens (the parts shown)
//   GET  /v1/badge/squeezed.svg, /v1/badge/not-loaded.svg   the token totals as badge images
//   GET  /v1/badge/squeezed, /v1/badge/not-loaded   the same as shields.io endpoint JSON
//
// Nothing here reads, logs or stores an IP address, a header or a GitHub login. The
// only data kept is what validate.js lets through: a random install id, a day,
// document counts per format and character sums.

import { MAX_BODY, parsePayload, rowHash } from "./validate.js";

const INSERT =
  "INSERT OR IGNORE INTO loads (h, id, day, docs, pdf, html, md, txt, src_bytes, raw, chars, lib_docs, lib_chars, tk, " +
  "chat, pdf_chars, html_chars, md_chars, txt_chars, chat_chars, vs_pdf_chars, vs_html_chars) " +
  "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)";
// Reading the converted text instead of the original costs fewer tokens. Measured with
// count_tokens on 85 reports (README, "What it saves"): raw HTML = 1.73x its converted text,
// a PDF read as a document = 2.2x. So per converted character, HTML saves 0.73 and PDF 1.2.
// Markdown, text and chats are read as they are: no conversion saving.
export const SAVED_PER_CHAR = { html: 0.73, pdf: 1.2 };
// Tokens are estimated per record with the characters-per-1000-tokens the app assumed (tk).
const SQUEEZE = "COALESCE(SUM(MAX(raw - chars, 0) * 1000.0 / tk), 0)";
// Counted against what the user would otherwise have read: version 3 records say (vs_*_chars),
// version 2 records count the loaded format, version 1 records (NULL characters) add nothing.
const VS_HTML = "COALESCE(vs_html_chars, html_chars, 0)";
const VS_PDF = "COALESCE(vs_pdf_chars, pdf_chars, 0)";
const CONVERT =
  `COALESCE(SUM((${VS_HTML} * ${SAVED_PER_CHAR.html} + ${VS_PDF} * ${SAVED_PER_CHAR.pdf}) * 1000.0 / tk), 0)`;
const STATS =
  // TOTAL, not SUM: an integer SUM that overflows is an error, and the stats would fail for good
  "SELECT COUNT(DISTINCT id) AS installs, COUNT(*) AS loads, TOTAL(docs) AS docs, " +
  "TOTAL(raw) AS raw_chars, TOTAL(chars) AS chars, " +
  `${SQUEEZE} AS tokens_from_squeeze, ${CONVERT} AS tokens_from_conversion, ` +
  "COALESCE(SUM(MAX(lib_chars - chars, 0) * 1000.0 / tk), 0) AS tokens_not_loaded, " +
  "COALESCE(SUM(chat > 0), 0) AS chat_loads, COUNT(pdf_chars) AS loads_with_formats FROM loads";
const BADGES = {
  squeezed: { label: "tokens squeezed", key: "tokens_squeezed", color: "brightgreen", hex: "#4c1" },
  "not-loaded": { label: "tokens not loaded", key: "tokens_not_loaded", color: "blue", hex: "#007ec6" },
};

// Rough Verdana 11px widths; textLength makes the text fit its box exactly either way.
function textWidth(s) {
  let w = 0;
  for (const c of s) w += c === " " || c === "." ? 3.6 : /[0-9]/.test(c) ? 7 : /[A-Z]/.test(c) ? 7.5 : /[ijlt]/.test(c) ? 3.8 : /[mw]/.test(c) ? 9.7 : 6.6;
  return Math.round(w);
}

const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

/** A flat badge like shields.io draws, so GitHub's image proxy needs no third site. */
export function badgeSvg(label, message, hex) {
  const lw = textWidth(label) + 10, mw = textWidth(message) + 10, w = lw + mw;
  const [l, m] = [esc(label), esc(message)];
  const text = (x, len, t) =>
    `<text x="${x * 10}" y="150" fill="#010101" fill-opacity=".3" transform="scale(.1)" textLength="${len * 10}">${t}</text>` +
    `<text x="${x * 10}" y="140" transform="scale(.1)" fill="#fff" textLength="${len * 10}">${t}</text>`;
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="20" role="img" aria-label="${l}: ${m}">` +
    `<title>${l}: ${m}</title>` +
    `<linearGradient id="s" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/><stop offset="1" stop-opacity=".1"/></linearGradient>` +
    `<clipPath id="r"><rect width="${w}" height="20" rx="3" fill="#fff"/></clipPath>` +
    `<g clip-path="url(#r)"><rect width="${lw}" height="20" fill="#555"/><rect x="${lw}" width="${mw}" height="20" fill="${hex}"/>` +
    `<rect width="${w}" height="20" fill="url(#s)"/></g>` +
    `<g fill="#fff" text-anchor="middle" font-family="Verdana,Geneva,DejaVu Sans,sans-serif" text-rendering="geometricPrecision" font-size="110">` +
    text(lw / 2, lw - 10, l) + text(lw + mw / 2, mw - 10, m) +
    `</g></svg>`
  );
}

/** 1234 -> "1.2k", 2712345 -> "2.7M": short enough for a badge. */
export function short(n) {
  const v = Math.round(n);
  for (const [d, unit] of [[1e9, "B"], [1e6, "M"], [1e3, "k"]]) {
    if (v >= d - d / 2000) {
      const x = Math.max(v / d, 1); // 999.5k rounds up: "1M", not "1000k"
      return `${x >= 100 ? Math.round(x) : Number(x.toFixed(1))}${unit}`;
    }
  }
  return String(v);
}
const CHUNK = 50; // statements per D1 batch

const HEADERS = { "x-content-type-options": "nosniff" };

function json(body, status = 200, extra = {}) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { ...HEADERS, "content-type": "application/json; charset=utf-8", ...extra },
  });
}

/** The request body as text, or null when it is bigger than the limit. */
async function readLimited(request, limit) {
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > limit) return null;
  if (!request.body) return "";
  const reader = request.body.getReader();
  const parts = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      return null;
    }
    parts.push(value);
  }
  const all = new Uint8Array(size);
  let at = 0;
  for (const p of parts) {
    all.set(p, at);
    at += p.byteLength;
  }
  return new TextDecoder().decode(all);
}

async function receive(request, env) {
  if (!(request.headers.get("content-type") || "").toLowerCase().startsWith("application/json")) {
    return json({ ok: false, error: "send application/json" }, 415);
  }
  const text = await readLimited(request, MAX_BODY);
  if (text === null) return json({ ok: false, error: "body too large" }, 413);
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    return json({ ok: false, error: "not JSON" }, 400);
  }
  const { rows, error } = parsePayload(body);
  if (error) return json({ ok: false, error }, 400);

  const statements = [];
  for (const r of rows) {
    const h = await rowHash(r);
    statements.push(
      env.DB.prepare(INSERT).bind(
        h, r.id, r.day, r.docs, r.pdf, r.html, r.md, r.txt, r.src_bytes, r.raw, r.chars, r.lib_docs, r.lib_chars, r.tk,
        r.chat, r.pdf_chars, r.html_chars, r.md_chars, r.txt_chars, r.chat_chars, r.vs_pdf_chars, r.vs_html_chars,
      ),
    );
  }
  let added = 0;
  for (let i = 0; i < statements.length; i += CHUNK) {
    const results = await env.DB.batch(statements.slice(i, i + CHUNK));
    for (const res of results) added += res.meta?.changes ?? 0;
  }
  return json({ ok: true, received: rows.length, new: added }, 200, { "cache-control": "no-store" });
}

async function totals(env) {
  const row = await env.DB.prepare(STATS).first();
  return {
    installs: row.installs,
    loads: row.loads,
    documents: row.docs,
    raw_chars: row.raw_chars,
    chars: row.chars,
    // tokens squeezed = the squeeze itself + reading text instead of the HTML/PDF original
    tokens_squeezed: Math.round(row.tokens_from_squeeze + row.tokens_from_conversion),
    tokens_from_squeeze: Math.round(row.tokens_from_squeeze),
    tokens_from_conversion: Math.round(row.tokens_from_conversion),
    tokens_not_loaded: Math.round(row.tokens_not_loaded),
    chat_loads: row.chat_loads,
    loads_with_formats: row.loads_with_formats, // loads that say which format their characters are
  };
}

async function stats(env) {
  return json(await totals(env), 200, { "cache-control": "public, max-age=300" });
}

// 300 s is the shortest cache shields.io accepts; the .svg badges use the same, so a new load
// shows on the repo page in about 5 to 10 minutes.
const BADGE_CACHE_SECONDS = 300;

/** shields.io "endpoint" JSON: https://img.shields.io/endpoint?url=<this URL> */
async function badge(env, which) {
  const b = BADGES[which];
  const t = await totals(env);
  return json(
    { schemaVersion: 1, label: b.label, message: short(t[b.key]), color: b.color, cacheSeconds: BADGE_CACHE_SECONDS },
    200,
    { "cache-control": `public, max-age=${BADGE_CACHE_SECONDS}` },
  );
}

/** The badge image itself. GitHub's image proxy gave up (504) waiting on shields.io for these. */
async function badgeImage(env, which) {
  const b = BADGES[which];
  const t = await totals(env);
  return new Response(badgeSvg(b.label, short(t[b.key]), b.hex), {
    headers: {
      ...HEADERS,
      "content-type": "image/svg+xml; charset=utf-8",
      "cache-control": `public, max-age=${BADGE_CACHE_SECONDS}`,
    },
  });
}

export default {
  async fetch(request, env) {
    const { pathname } = new URL(request.url);
    if (pathname === "/v1/usage") {
      if (request.method !== "POST") return json({ ok: false, error: "POST only" }, 405, { allow: "POST" });
      // Optional rate limit (see wrangler.toml). The IP is only the key of the limiter; it is never stored here.
      if (env.RATE) {
        const { success } = await env.RATE.limit({ key: request.headers.get("cf-connecting-ip") || "unknown" });
        if (!success) return json({ ok: false, error: "slow down" }, 429, { "retry-after": "60" });
      }
      return receive(request, env);
    }
    if (pathname === "/v1/stats") {
      if (request.method !== "GET") return json({ ok: false, error: "GET only" }, 405, { allow: "GET" });
      return stats(env);
    }
    if (pathname.startsWith("/v1/badge/") && pathname.endsWith(".svg") && Object.hasOwn(BADGES, pathname.slice(10, -4))) {
      if (request.method !== "GET") return json({ ok: false, error: "GET only" }, 405, { allow: "GET" });
      return badgeImage(env, pathname.slice(10, -4));
    }
    if (pathname.startsWith("/v1/badge/") && Object.hasOwn(BADGES, pathname.slice(10))) {
      if (request.method !== "GET") return json({ ok: false, error: "GET only" }, 405, { allow: "GET" });
      return badge(env, pathname.slice(10));
    }
    if (pathname === "/") {
      return new Response("the-researcher usage receiver: anonymous counts only. https://github.com/LudaLunizz/the-researcher\n", {
        headers: { ...HEADERS, "content-type": "text/plain; charset=utf-8" },
      });
    }
    return json({ ok: false, error: "not found" }, 404);
  },
};
