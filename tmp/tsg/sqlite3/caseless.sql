-- lower case keywords exercise %lexer_caseless
create table things (id integer primary key, name text);
insert into things (id, name) values (1, 'one');
select id, name from things where id >= 1 order by name desc;
/* block comment */
update things set name = 'two' where id = 1;
