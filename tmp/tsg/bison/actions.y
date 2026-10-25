%{
#include <stdio.h>
%}

%define api.value.type union
%union { int number; }
%token <number> NUM
%left '+'

%start unit

%%
unit: %empty      { count = 0; }
    | unit stmt   { count += 1; }
    ;

stmt: expr ';'    { if ($1) { total += $1; } }
    ;

expr: NUM         { $$ = $1; }
    | expr '+' expr { $$ = $1 + $3; }
    ;
%%
