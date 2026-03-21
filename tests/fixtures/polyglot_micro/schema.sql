-- Minimal SQL file for polyglot test fixture
-- This file provides a simple function to verify SQL node extraction

CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    username TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT now()
);

CREATE FUNCTION get_user_count()
RETURNS INTEGER
LANGUAGE sql
AS $$
    SELECT COUNT(*) FROM users;
$$;
