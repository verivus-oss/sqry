-- Hand-written SQL fixture for the body-shape descriptor coverage test.
-- A LANGUAGE sql function body is parsed structurally by tree-sitter-sequel,
-- so the CASE expression (and its WHEN branches) shows up as real control flow.

CREATE FUNCTION grade(score integer, bonus integer DEFAULT 0) RETURNS text AS $$
  SELECT CASE
    WHEN score + bonus >= 90 THEN 'A'
    WHEN score + bonus >= 80 THEN 'B'
    ELSE 'C'
  END
$$ LANGUAGE sql;
