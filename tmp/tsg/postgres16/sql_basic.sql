SELECT a, b::text, count(*) AS n FROM t WHERE x = 1 AND y LIKE 'foo%' GROUP BY a, b ORDER BY n DESC LIMIT 10;
CREATE TABLE foo (id serial PRIMARY KEY, name text NOT NULL DEFAULT 'x', ts timestamptz);
INSERT INTO foo (name) VALUES ('a'), ('b') RETURNING id;
UPDATE foo SET name = 'c' WHERE id = 1;
DELETE FROM foo;
BEGIN; COMMIT;
CREATE INDEX CONCURRENTLY idx ON foo (name);
