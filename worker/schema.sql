-- One row per load. No names, no paths, no IP addresses, no GitHub login.
-- `h` is a hash of the numbers, so a record that is sent twice is stored once.
CREATE TABLE IF NOT EXISTS loads (
  h         TEXT PRIMARY KEY,
  id        TEXT NOT NULL,    -- random install id (16 hex), reset by deleting the usage-id file
  day       TEXT NOT NULL,    -- YYYY-MM-DD, local day of the load
  docs      INTEGER NOT NULL,
  pdf       INTEGER NOT NULL,
  html      INTEGER NOT NULL,
  md        INTEGER NOT NULL,
  txt       INTEGER NOT NULL,
  src_bytes INTEGER NOT NULL,
  raw       INTEGER NOT NULL, -- characters before squeezing
  chars     INTEGER NOT NULL, -- characters loaded
  lib_docs  INTEGER NOT NULL,
  lib_chars INTEGER NOT NULL,
  tk        INTEGER NOT NULL, -- characters per 1000 tokens the app assumed
  -- record version 2: chats loaded as context, and the loaded characters per format
  -- (NULL = a version 1 record that did not say). Same columns as migrations/0002_formats.sql.
  chat       INTEGER NOT NULL DEFAULT 0,
  pdf_chars  INTEGER,
  html_chars INTEGER,
  md_chars   INTEGER,
  txt_chars  INTEGER,
  chat_chars INTEGER,
  -- record version 3: the characters counted against PDF / HTML, the format the user would
  -- otherwise have read (NULL = count pdf_chars / html_chars). Same as migrations/0003_vs.sql.
  vs_pdf_chars  INTEGER,
  vs_html_chars INTEGER
);
CREATE INDEX IF NOT EXISTS loads_day ON loads (day);
CREATE INDEX IF NOT EXISTS loads_id ON loads (id);
