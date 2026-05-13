---
title: Functions
order: 38
---

# Functions

AionDB implements a growing set of scalar functions. This page lists the main user-facing families.

Function compatibility should be tested by behavior, not only by name. PostgreSQL has many edge cases around nulls, encodings, regex flags, timezone rules, formatting strings, and implicit casts.

## Text functions

Common text functions include:

- `upper`
- `lower`
- `length`
- `char_length`
- `octet_length`
- `substring`
- `substr`
- `trim`
- `ltrim`
- `rtrim`
- `replace`
- `strpos`
- `left`
- `right`
- `repeat`
- `reverse`
- `starts_with`
- `concat`
- `concat_ws`
- `format`
- `split_part`
- `translate`
- `overlay`
- `bit_length`
- `chr`
- `ascii`
- `md5`
- `quote_literal`
- `quote_ident`
- `quote_nullable`
- `to_hex`

Example:

```sql
SELECT lower(name), length(name)
FROM users;
```

Useful text checks:

```sql
SELECT upper('aiondb');
SELECT substring('abcdef' FROM 2 FOR 3);
SELECT replace('a-b-c', '-', '_');
SELECT concat_ws('/', 'docs', 'query', 'functions');
```

Test null handling if the application depends on PostgreSQL-equivalent behavior.

## Regular expression functions

The text function registry includes PostgreSQL-style regular expression helpers such as:

- `regexp_replace`
- `regexp_match`
- `regexp_matches`
- `regexp_split_to_array`
- `regexp_split_to_table`

Regex behavior is compatibility-sensitive. Test flags and edge cases against your expected PostgreSQL behavior.

Recommended regex fixture:

```sql
SELECT regexp_replace('abc123', '[0-9]+', 'N');
SELECT regexp_match('abc123', '([a-z]+)([0-9]+)');
```

## Date and time functions

Common date/time functions include:

- `now`
- `current_timestamp`
- `current_date`
- `current_time`
- `localtime`
- `date_part`
- `extract`
- `date_trunc`
- `age`
- `to_char`
- `to_date`
- `to_timestamp`
- `make_date`
- `make_time`
- `make_timestamp`
- `make_interval`
- `clock_timestamp`
- `statement_timestamp`
- `transaction_timestamp`
- `timezone`

Date/time functions are among the most compatibility-sensitive surfaces. Validate timezone, precision, formatting, and transaction timestamp expectations with your driver.

Useful checks:

```sql
SELECT current_date;
SELECT date_part('year', current_timestamp);
SELECT date_trunc('day', current_timestamp);
```

## Vector functions

```sql
SELECT l2_distance(embedding, '[1.0,0.0,0.0]') FROM items;
SELECT cosine_distance(embedding, '[1.0,0.0,0.0]') FROM items;
```

Vector functions require matching dimensions.

Keep a tiny deterministic fixture:

```sql
CREATE TABLE vector_fn_demo (
    id INT,
    embedding VECTOR(2)
);

INSERT INTO vector_fn_demo VALUES
    (1, '[0.0,0.0]'),
    (2, '[1.0,0.0]');

SELECT id, l2_distance(embedding, '[1.0,0.0]') AS dist
FROM vector_fn_demo
ORDER BY dist ASC;
```

## Compatibility functions

AionDB includes PostgreSQL-facing helper functions used by drivers, catalog queries, and compatibility paths. Treat those as implementation compatibility, not as a complete PostgreSQL extension surface.

## Reporting function gaps

Function bug reports should include:

- function name;
- argument types;
- SQL text;
- expected output or PostgreSQL reference output;
- actual output or SQLSTATE;
- whether the query used parameters.
