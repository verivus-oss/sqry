-- Sample schema exercising INSTEAD OF triggers and ONLY table references.
-- This fixture is used by Phase 3 Sprint 2 SQL graph tests.

CREATE TABLE public.accounts (
    id              BIGSERIAL PRIMARY KEY,
    customer_id     BIGINT NOT NULL,
    balance_cents   BIGINT NOT NULL DEFAULT 0,
    updated_at      TIMESTAMP WITH TIME ZONE DEFAULT now()
);

CREATE TABLE public.archived_accounts (
    id              BIGINT PRIMARY KEY,
    customer_id     BIGINT NOT NULL,
    balance_cents   BIGINT NOT NULL,
    archived_at     TIMESTAMP WITH TIME ZONE DEFAULT now()
);

CREATE VIEW public.v_customer_balances AS
SELECT
    a.customer_id,
    a.balance_cents
FROM
    public.accounts AS a;

CREATE FUNCTION public.sync_customer_balance()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE ONLY public.accounts
       SET balance_cents = NEW.balance_cents,
           updated_at    = now()
     WHERE customer_id  = NEW.customer_id;

    RETURN NEW;
END;
$$;

CREATE TRIGGER customer_balance_update
INSTEAD OF UPDATE ON public.v_customer_balances
FOR EACH ROW
EXECUTE FUNCTION public.sync_customer_balance();

-- Note: tree-sitter-sequel grammar supports CREATE FUNCTION but not CREATE PROCEDURE.
-- Using CREATE FUNCTION with plpgsql instead (functionally equivalent in Postgres).
CREATE FUNCTION public.close_account(p_customer BIGINT)
RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO public.archived_accounts (id, customer_id, balance_cents)
    SELECT id, customer_id, balance_cents
      FROM ONLY public.accounts
     WHERE customer_id = p_customer;

    DELETE FROM ONLY public.accounts
     WHERE customer_id = p_customer;
END;
$$;

-- Note: CALL statement is not supported by tree-sitter-sequel grammar.
-- In real Postgres, you would invoke with: SELECT public.close_account(42);
-- For testing purposes, the function definition is sufficient to verify graph extraction.
