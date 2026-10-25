CREATE TABLE select (from INTEGER, where TEXT);
INSERT INTO select (from, where) VALUES (1, 'x');
SELECT from, where FROM select;
SELECT t.from FROM select AS t;
