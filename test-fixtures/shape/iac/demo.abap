* Hand-written ABAP sample for body-shape descriptor coverage.
* Exercises the control-flow constructs the tree-sitter-abap-sqry grammar parses
* as named nodes: if/elseif branch, LOOP iteration, TRY/CATCH, RAISE, method/
* function calls, assignment, EXIT/CHECK, and RETURN.
CLASS zcl_order_processor DEFINITION.
  PUBLIC SECTION.
    METHODS process
      IMPORTING iv_count TYPE i
      RETURNING VALUE(rv_total) TYPE i.
ENDCLASS.

CLASS zcl_order_processor IMPLEMENTATION.
  METHOD process.
    DATA lv_running TYPE i.
    lv_running = iv_count.

    IF lv_running > 0.
      rv_total = lv_running.
    ELSEIF lv_running < 0.
      rv_total = 0.
    ENDIF.

    LOOP AT gt_orders INTO DATA(ls_order).
      CHECK ls_order-active = abap_true.
      rv_total = rv_total + ls_order-amount.
      me->audit( rv_total ).
    ENDLOOP.

    TRY.
        rv_total = me->compute( lv_running ).
      CATCH cx_root INTO DATA(lo_err).
        RAISE EXCEPTION lo_err.
    ENDTRY.

    RETURN.
  ENDMETHOD.
ENDCLASS.
