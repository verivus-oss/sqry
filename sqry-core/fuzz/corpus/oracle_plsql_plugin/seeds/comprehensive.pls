CREATE OR REPLACE PACKAGE employee_pkg IS
    PROCEDURE add_employee(p_name VARCHAR2, p_salary NUMBER);
    FUNCTION get_employee_count RETURN NUMBER;
END employee_pkg;
/

CREATE OR REPLACE PACKAGE BODY employee_pkg IS
    PROCEDURE add_employee(p_name VARCHAR2, p_salary NUMBER) IS
    BEGIN
        INSERT INTO employees (name, salary) VALUES (p_name, p_salary);
    END add_employee;

    FUNCTION get_employee_count RETURN NUMBER IS
        v_count NUMBER;
    BEGIN
        SELECT COUNT(*) INTO v_count FROM employees;
        RETURN v_count;
    END get_employee_count;
END employee_pkg;
/
