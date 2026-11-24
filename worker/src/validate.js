// Strict validation of the anonymous usage records the researcher writes. Only these
// numbers are accepted; any other key, any string that is not the expected shape, and any
// out-of-range number makes the whole request fail, so free text (titles, paths, names)
// can never be stored.
//
// Two request shapes:
//   schema 1: the file `researcher --usage-export` writes (records as logged);
//   schema 2: what `researcher --usage-send` sends: every record also has `seq`, its line
//             number in the user's log, so two identical loads on one day are both kept.
// Two record versions (`v`):
//   1: counts per format (pdf, html, md, txt) and character sums;
//   2: also `chat` (earlier Claude Code chats loaded as context) in `fmt`, and `fmt_chars`,
//      the loaded characters per format, which is what the conversion saving needs;
//   3: also `vs_chars`: the same characters per format the user would otherwise have read
//      (`[usage] before` in the app when the document exists in that format, else the loaded
//      file's own format). The conversion saving is counted against these.

export const SCHEMA = 3; // newest record version (`v`)
const VERSIONS = [1, 2, 3];
export const SCHEMAS = [1, 2]; // request versions (`schema`)
const SEQ_MAX = 1_000_000_000;
export const MAX_LOADS = 200; // records per request
export const MAX_BODY = 262144; // bytes per request

const KINDS_V1 = ["pdf", "html", "md", "txt"];
const KINDS_V2 = [...KINDS_V1, "chat"];
const LOAD_KEYS = ["chars", "day", "docs", "fmt", "id", "lib_chars", "lib_docs", "raw", "src_bytes", "tk", "v"];
/** The keys a record must have: version 2 adds fmt_chars, 3 vs_chars, request schema 2 adds seq. */
const keysFor = (v, withSeq) =>
  [...LOAD_KEYS, ...(v >= 2 ? ["fmt_chars"] : []), ...(v >= 3 ? ["vs_chars"] : []), ...(withSeq ? ["seq"] : [])].sort();
const COUNT_MAX = 1_000_000; // documents
const BYTES_MAX = 10_000_000_000; // source bytes (not in the stats)
const SIZE_MAX = 1_000_000_000; // characters: they feed the public token numbers
const DAY_FIRST = Date.UTC(2026, 0, 1);
const DAY_SLACK = 2 * 86_400_000; // clocks and time zones

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const isInt = (v, max) => Number.isSafeInteger(v) && v >= 0 && v <= max;
const sameKeys = (obj, keys) => {
  const have = Object.keys(obj).sort();
  return have.length === keys.length && have.every((k, i) => k === keys[i]);
};

function validDay(s, now) {
  if (typeof s !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(s)) return false;
  const t = Date.parse(`${s}T00:00:00Z`);
  if (Number.isNaN(t) || new Date(t).toISOString().slice(0, 10) !== s) return false;
  return t >= DAY_FIRST && t <= now + DAY_SLACK;
}

/** One record -> error text, or null when it is fine. `withSeq`: request schema 2. */
export function recordError(rec, now = Date.now(), withSeq = false) {
  if (!isObject(rec)) return "unexpected fields";
  if (!VERSIONS.includes(rec.v)) return "unsupported record version";
  if (!sameKeys(rec, keysFor(rec.v, withSeq))) return "unexpected fields";
  if (withSeq && (!Number.isSafeInteger(rec.seq) || rec.seq < 1 || rec.seq > SEQ_MAX)) return "bad seq";
  if (typeof rec.id !== "string" || !/^[0-9a-f]{16}$/.test(rec.id)) return "bad id";
  if (!validDay(rec.day, now)) return "bad day";
  if (!isInt(rec.docs, COUNT_MAX) || !isInt(rec.lib_docs, COUNT_MAX)) return "bad document count";
  if (!isInt(rec.src_bytes, BYTES_MAX)) return "bad src_bytes";
  for (const k of ["raw", "chars", "lib_chars"]) {
    if (!isInt(rec[k], SIZE_MAX)) return `bad ${k}`;
  }
  // characters per 1000 tokens (the app sends 2400): the stats divide by it, so a tiny one
  // would let one record add billions of tokens
  if (!Number.isSafeInteger(rec.tk) || rec.tk < 1000 || rec.tk > 10_000) return "bad tk";
  const kinds = rec.v >= 2 ? KINDS_V2 : KINDS_V1;
  if (!isObject(rec.fmt) || !sameKeys(rec.fmt, [...kinds].sort())) return "bad fmt";
  let sum = 0;
  for (const k of kinds) {
    if (!isInt(rec.fmt[k], COUNT_MAX)) return "bad fmt count";
    sum += rec.fmt[k];
  }
  if (sum !== rec.docs) return "fmt does not add up to docs";
  if (rec.v >= 2) {
    if (!isObject(rec.fmt_chars) || !sameKeys(rec.fmt_chars, [...kinds].sort())) return "bad fmt_chars";
    let chars = 0;
    for (const k of kinds) {
      if (!isInt(rec.fmt_chars[k], SIZE_MAX)) return "bad fmt_chars count";
      chars += rec.fmt_chars[k];
    }
    if (chars !== rec.chars) return "fmt_chars does not add up to chars";
  }
  if (rec.v >= 3) {
    if (!isObject(rec.vs_chars) || !sameKeys(rec.vs_chars, [...kinds].sort())) return "bad vs_chars";
    let chars = 0;
    for (const k of kinds) {
      if (!isInt(rec.vs_chars[k], SIZE_MAX)) return "bad vs_chars count";
      chars += rec.vs_chars[k];
    }
    if (chars !== rec.chars) return "vs_chars does not add up to chars";
    // a chat is read as it is: it is never counted against a file format
    if (rec.vs_chars.chat !== rec.fmt_chars.chat) return "vs_chars chat differs from fmt_chars";
  }
  return null;
}

/** The request body (parsed JSON) -> {rows} or {error}. */
export function parsePayload(body, now = Date.now()) {
  if (!isObject(body) || !sameKeys(body, ["loads", "schema"])) return { error: "unexpected fields" };
  if (!SCHEMAS.includes(body.schema)) return { error: "unsupported schema" };
  const withSeq = body.schema === 2;
  if (!Array.isArray(body.loads) || body.loads.length === 0) return { error: "no records" };
  if (body.loads.length > MAX_LOADS) return { error: `more than ${MAX_LOADS} records` };
  const rows = [];
  for (const [i, rec] of body.loads.entries()) {
    const err = recordError(rec, now, withSeq);
    if (err) return { error: `record ${i}: ${err}` };
    const v2 = rec.v >= 2;
    const v3 = rec.v >= 3;
    rows.push({
      id: rec.id,
      day: rec.day,
      docs: rec.docs,
      pdf: rec.fmt.pdf,
      html: rec.fmt.html,
      md: rec.fmt.md,
      txt: rec.fmt.txt,
      src_bytes: rec.src_bytes,
      raw: rec.raw,
      chars: rec.chars,
      lib_docs: rec.lib_docs,
      lib_chars: rec.lib_chars,
      tk: rec.tk,
      seq: withSeq ? rec.seq : null,
      // version 1 records do not say: NULL characters, no chats
      chat: v2 ? rec.fmt.chat : 0,
      pdf_chars: v2 ? rec.fmt_chars.pdf : null,
      html_chars: v2 ? rec.fmt_chars.html : null,
      md_chars: v2 ? rec.fmt_chars.md : null,
      txt_chars: v2 ? rec.fmt_chars.txt : null,
      chat_chars: v2 ? rec.fmt_chars.chat : null,
      // version 3: the characters counted against PDF / HTML (NULL = count fmt_chars)
      vs_pdf_chars: v3 ? rec.vs_chars.pdf : null,
      vs_html_chars: v3 ? rec.vs_chars.html : null,
    });
  }
  return { rows };
}

/**
 * A hash of the record's numbers: sending the same record again changes nothing. With a
 * `seq` (schema 2) it is part of the hash, so two identical loads on one day are two rows;
 * without one (schema 1) the hash is the same as before seq existed. Version 2 records add
 * their chat count and per-format characters, version 3 what they are counted against; version 1
 * and 2 hashes are unchanged.
 */
export async function rowHash(row) {
  const parts = [
    row.id, row.day, row.docs, row.pdf, row.html, row.md, row.txt,
    row.src_bytes, row.raw, row.chars, row.lib_docs, row.lib_chars, row.tk,
  ];
  if (row.seq != null) parts.push(`seq${row.seq}`);
  if (row.pdf_chars != null) {
    parts.push(`chat${row.chat}`, `fc${row.pdf_chars},${row.html_chars},${row.md_chars},${row.txt_chars},${row.chat_chars}`);
  }
  if (row.vs_pdf_chars != null) parts.push(`vs${row.vs_pdf_chars},${row.vs_html_chars}`);
  const text = parts.join("|");
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
