CREATE VIRTUAL TABLE f USING fts5(a, b, tokenize = 'porter');
SELECT * FROM f;
