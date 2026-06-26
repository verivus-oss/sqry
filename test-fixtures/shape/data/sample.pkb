-- Hand-written Oracle PL/SQL package body fixture for the body-shape coverage
-- test. A package body parses the procedure/function definitions cleanly, so
-- the procedural control flow (IF, loops, CASE, exception handling, RAISE) is
-- available as real grammar nodes the shape walker can count.

CREATE OR REPLACE PACKAGE BODY grading AS

  PROCEDURE classify(score IN NUMBER, bonus IN NUMBER DEFAULT 0, grade OUT VARCHAR2) IS
    total NUMBER := 0;
    i     NUMBER := 0;
  BEGIN
    total := score + bonus;

    IF total >= 90 THEN
      grade := 'A';
    ELSIF total >= 80 THEN
      grade := 'B';
    ELSE
      grade := 'C';
    END IF;

    FOR i IN 1 .. 3 LOOP
      log_it(i);
    END LOOP;

    WHILE i < 5 LOOP
      i := i + 1;
    END LOOP;

    CASE grade
      WHEN 'A' THEN log_it(100);
      ELSE log_it(0);
    END CASE;
  EXCEPTION
    WHEN OTHERS THEN
      RAISE;
  END classify;

  FUNCTION letter(score IN NUMBER) RETURN VARCHAR2 IS
  BEGIN
    IF score >= 90 THEN
      RETURN 'A';
    END IF;
    RETURN 'F';
  END letter;

END grading;
