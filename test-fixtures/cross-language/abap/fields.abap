CLASS zcl_ledger DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: mutable_field TYPE i.
    CONSTANTS: immutable_field TYPE i VALUE 1.
    CLASS-DATA: static_field TYPE i.
  PRIVATE SECTION.
    DATA: private_field TYPE string.
  PUBLIC SECTION.
    DATA: shared_name TYPE i.
    METHODS read_ledger.
ENDCLASS.

CLASS zcl_archive DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: shared_name TYPE i.
ENDCLASS.

CLASS zcl_ledger IMPLEMENTATION.
  METHOD read_ledger.
    DATA mutable_field TYPE i.
    mutable_field = me->mutable_field.
  ENDMETHOD.
ENDCLASS.
