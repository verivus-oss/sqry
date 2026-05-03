* SPDX-License-Identifier: MIT
CLASS zcl_session_state DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: mutable_field TYPE string.
    CONSTANTS: immutable_field TYPE string VALUE 'fixed'.
    CLASS-DATA: static_field TYPE i.
    DATA: shared_name TYPE i.
  PRIVATE SECTION.
    DATA: private_field TYPE string.
ENDCLASS.

CLASS zcl_audit_state DEFINITION PUBLIC.
  PUBLIC SECTION.
    DATA: shared_name TYPE i.
ENDCLASS.
