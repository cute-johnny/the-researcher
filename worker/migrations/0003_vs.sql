-- For a database made before record version 3 (researcher 0.7): the characters counted against
-- PDF / HTML, the format the user would otherwise have read. Old rows keep NULL (the stats then
-- count pdf_chars / html_chars). Run once:
--   npx wrangler d1 execute researcher-usage --remote --file migrations/0003_vs.sql
ALTER TABLE loads ADD COLUMN vs_pdf_chars INTEGER;
ALTER TABLE loads ADD COLUMN vs_html_chars INTEGER;
