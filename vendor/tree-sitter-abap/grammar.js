module.exports = grammar({
  name: "abap",

  word: $ => $.name,

  extras: $ => [/\s+/, $.eol_comment, $.bol_comment],

  rules: {
    program: $ => repeat($._statement),

    _statement: $ =>
      choice(
        $.class_declaration,
        $.class_implementation,
        $.class_publication,
        $.class_local_friend_publication,
        $.interface_declaration,
        $.function_implementation,
        $._implementation_statement
      ),

    _implementation_statement: $ =>
      choice(
        $.variable_declaration,
        $.chained_variable_declaration,
        $.chained_structure_declaration,
        $.loop_statement,
        $.field_symbol_declaration,
        $.chained_field_symbol_declaration,
        $.exit_statement,
        $.continue_statement,
        $.report_statement,
        $.if_statement,
        $.return_statement,
        $.check_statement,
        $.assignment,
        $.select_statement_obsolete,
        $.read_table_statement,
        $.try_catch_statement,
        $.write_statement,
        $.chained_write_statement,
        $.call_method,
        $.call_method_static,
        $.call_method_instance,
        $.call_function,
        $.raise_exception_statement,
        $.clear_statement,
        $.append_statement,
        $.create_object_statement,
        $.include_statement,
        $.macro_include,
        $.raise_statement,
        $.append_statement_obsolete
      ),

    class_declaration: $ =>
      seq(
        kw("class"),
        field("name", $.name),
        kw("definition"),
        optional(kw("public")),
        optional(
          seq(kw("inheriting"), kw("from"), field("superclass", $.name))
        ),
        optional(kw("abstract")),
        optional(kw("final")),
        optional($._create_addition),
        optional(seq(kw("shared"), kw("memory"), kw("enabled"))),
        optional(
          seq(
            optional(kw("global")),
            kw("friends"),
            field("friends", repeat1($.name))
          )
        ),
        ".",
        optional($.public_section),
        optional($.protected_section),
        optional($.private_section),
        kw("endclass"),
        "."
      ),

    _create_addition: $ =>
      seq(kw("create"), choice(kw("public"), kw("protected"), kw("private"))),

    public_section: $ =>
      seq(kw("public"), kw("section"), ".", repeat($._class_components)),

    protected_section: $ =>
      seq(kw("protected"), kw("section"), ".", repeat($._class_components)),

    private_section: $ =>
      seq(kw("private"), kw("section"), ".", repeat($._class_components)),

    _class_components: $ =>
      choice(
        $.variable_declaration,
        alias($.method_declaration_class, $.method_declaration),
        $.constructor_declaration,
        $.method_redefinition,
        alias($.class_method_declaration_class, $.class_method_declaration),
        $.class_constructor_declaration
      ),

    class_implementation: $ =>
      seq(
        kw("class"),
        field("name", $.name),
        kw("implementation"),
        ".",
        repeat($.method_implementation),
        kw("endclass"),
        "."
      ),

    method_declaration_class: $ =>
      seq(
        kw("methods"),
        field("name", $.name),
        optional(choice(kw("abstract"), kw("final"))),
        field(
          "importing_parameters",
          optional($._method_declaration_importing)
        ),
        field(
          "exporting_parameters",
          optional($._method_declaration_exporting)
        ),
        field("changing_parameters", optional($._method_declaration_changing)),
        optional($.returning_parameter),
        field("raising", optional($._method_declaration_raising)),
        field("exceptions", optional($._method_declaration_exceptions)),
        "."
      ),

    _method_declaration_importing: $ =>
      seq(kw("importing"), repeat1($.method_parameters)),

    _method_declaration_exporting: $ =>
      seq(kw("exporting"), repeat1($.method_parameters)),

    _method_declaration_changing: $ =>
      seq(kw("changing"), repeat1($.method_parameters)),

    _method_declaration_raising: $ =>
      seq(
        kw("raising"),
        repeat1(choice($.name, seq(kw("resumable"), "(", $.name, ")")))
      ),

    _method_declaration_exceptions: $ => seq(kw("exceptions"), repeat1($.name)),

    method_parameters: $ =>
      seq(
        choice(
          seq(kw("value"), "(", $.name, ")"),
          seq(kw("reference"), "(", $.name, ")"),
          $._operand
        ),
        $._typing,
        optional(choice(kw("optional"), seq(kw("default"), $.name)))
      ),

    returning_parameter: $ =>
      seq(kw("returning"), kw("value"), "(", $.name, ")", $.complete_typing),

    constructor_declaration: $ =>
      seq(
        kw("methods"),
        kw("constructor"),
        optional(kw("final")),
        field(
          "importing_parameters",
          optional($._method_declaration_importing)
        ),
        field("raising", optional($._method_declaration_raising)),
        field("exceptions", optional($._method_declaration_exceptions)),
        "."
      ),

    class_constructor_declaration: $ =>
      seq(kw("class-methods"), kw("class_constructor"), "."),

    method_redefinition: $ =>
      seq(
        kw("methods"),
        $.name,
        optional(kw("final")),
        kw("redefinition"),
        "."
      ),

    class_method_declaration_class: $ =>
      seq(
        kw("class-methods"),
        field("name", $.name),
        field(
          "importing_parameters",
          optional($._method_declaration_importing)
        ),
        field(
          "exporting_parameters",
          optional($._method_declaration_exporting)
        ),
        field("changing_parameters", optional($._method_declaration_changing)),
        optional($.returning_parameter),
        field("raising", optional($._method_declaration_raising)),
        field("exceptions", optional($._method_declaration_exceptions)),
        "."
      ),

    class_method_declaration_interface: $ =>
      seq(
        kw("class-methods"),
        field("name", $.name),
        optional(seq(kw("default"), choice(kw("ignore"), kw("fail")))),
        field(
          "importing_parameters",
          optional($._method_declaration_importing)
        ),
        field(
          "exporting_parameters",
          optional($._method_declaration_exporting)
        ),
        field("changing_parameters", optional($._method_declaration_changing)),
        optional($.returning_parameter),
        field("raising", optional($._method_declaration_raising)),
        field("exceptions", optional($._method_declaration_exceptions)),
        "."
      ),

    method_implementation: $ =>
      seq(
        kw("method"),
        field("name", $.name),
        ".",
        optional($.method_body),
        kw("endmethod"),
        "."
      ),

    method_body: $ => repeat1($._implementation_statement),

    class_publication: $ =>
      seq(
        kw("class"),
        field("name", $.name),
        kw("definition"),
        kw("deferred"),
        optional(kw("public")),
        "."
      ),

    class_local_friend_publication: $ =>
      seq(
        kw("class"),
        field("name", $.name),
        kw("definition"),
        kw("local"),
        kw("friends"),
        field("friends", repeat1($.name)),
        "."
      ),

    interface_declaration: $ =>
      seq(
        kw("interface"),
        field("name", $.name),
        optional(kw("public")),
        ".",
        repeat($._interface_components),
        kw("endinterface"),
        "."
      ),

    _interface_components: $ =>
      choice(
        $.variable_declaration,
        alias($.method_declaration_interface, $.method_declaration),
        alias($.class_method_declaration_interface, $.class_method_declaration)
      ),

    method_declaration_interface: $ =>
      seq(
        kw("methods"),
        field("name", $.name),
        optional(seq(kw("default"), choice(kw("ignore"), kw("fail")))),
        field(
          "importing_parameters",
          optional($._method_declaration_importing)
        ),
        field(
          "exporting_parameters",
          optional($._method_declaration_exporting)
        ),
        field("changing_parameters", optional($._method_declaration_changing)),
        optional($.returning_parameter),
        field("raising", optional($._method_declaration_raising)),
        field("exceptions", optional($._method_declaration_exceptions)),
        "."
      ),

    _typing: $ => choice($.generic_typing, $.complete_typing),

    generic_typing: $ =>
      choice(seq(kw("type"), $.generic_type), seq(kw("like"), $.name)),

    complete_typing: $ =>
      choice(
        seq(kw("type"), alias($.name, $.type)),
        seq(kw("type"), kw("ref"), kw("to"), alias($.name, $.type))
      ),

    generic_type: $ => choice(kw("any"), seq(kw("any"), kw("table"))),

    _data_object_typing: $ =>
      choice(
        //$._data_object_typing_built_in,
        $._data_object_typing_normal,
        //$._data_object_typing_enumerated,
        $._data_object_typing_reference,
        $._data_object_typing_itabs
        //$._data_object_typing_ranges
      ),

    _data_object_typing_normal: $ =>
      seq(
        choice(
          seq(
            kw("type"),
            optional(seq(kw("line"), kw("of"))),
            alias($.name, $.type)
          ),
          seq(kw("like"), optional(seq(kw("line"), kw("of"))), $._data_object)
        ),
        optional(
          seq(kw("value"), choice($.name, seq(kw("is"), kw("initial"))))
        ),
        optional(kw("read-only"))
      ),

    _data_object_typing_reference: $ =>
      seq(
        choice(
          seq(kw("type"), kw("ref"), kw("to"), alias($.name, $.type)),
          seq(kw("like"), kw("ref"), kw("to"), $.name)
        ),
        optional(seq(kw("value"), kw("is"), kw("initial"))),
        optional(kw("read-only"))
      ),

    _data_object_typing_itabs: $ =>
      seq(
        choice(
          seq(
            kw("type"),
            choice(optional(kw("standard")), kw("sorted"), kw("hashed")),
            kw("table"),
            kw("of"),
            optional(seq(kw("ref"), kw("to"))),
            alias($.name, $.type)
          ),
          seq(
            kw("like"),
            choice(optional(kw("standard")), kw("sorted"), kw("hashed")),
            kw("table"),
            kw("of"),
            $.name
          )
        )
      ),

    variable_declaration: $ =>
      seq(
        kw("data"),
        field("name", $.name),
        field("typing", alias($._data_object_typing, $.typing)),
        "."
      ),

    chained_variable_declaration: $ =>
      seq(
        kw("data"),
        ":",
        repeat1(choice($.variable, seq(",", $.variable))),
        "."
      ),

    variable: $ => seq($.name, alias($._data_object_typing, $.typing)),

    chained_structure_declaration: $ =>
      seq(
        kw("data"),
        ":",
        kw("begin"),
        kw("of"),
        alias($.name, $.strucure_name),
        optional(kw("read-only")),
        ",",
        alias(repeat1($.structure_component), $.structure_components),
        kw("end"),
        kw("of"),
        alias($.name, $.strucure_name),
        "."
      ),

    structure_component: $ => seq($.name, $._typing, ","),

    field_symbol_declaration: $ =>
      seq(
        kw("field-symbols"),
        alias($.field_symbol_name, $.name),
        $._typing,
        "."
      ),

    chained_field_symbol_declaration: $ =>
      seq(
        kw("field-symbols"),
        ":",
        repeat1(choice($.field_symbol, seq(",", $.field_symbol))),
        "."
      ),

    field_symbol: $ => seq(alias($.field_symbol_name, $.name), $._typing),

    loop_statement: $ =>
      seq(
        kw("loop"),
        kw("at"),
        alias($.name, $.itab),
        choice(
          seq(kw("into"), alias($.name, $.result)),
          seq(kw("assigning"), alias($.field_symbol_name, $.result))
        ),
        optional(
          seq(
            optional(seq(kw("from"), $._general_expression_position)),
            optional(seq(kw("to"), $._general_expression_position)),
            optional(seq(kw("step"), $._general_expression_position))
          )
        ),
        ".",
        // FIXME: not all statements are allowed in loop body
        repeat($._statement),
        kw("endloop"),
        "."
      ),

    exit_statement: $ => seq(kw("exit"), "."),

    continue_statement: $ => seq(kw("continue"), "."),

    return_statement: $ => seq(kw("return"), "."),

    report_statement: $ => seq(kw("report"), $.name, "."),

    if_statement: $ =>
      seq(
        kw("if"),
        $._logical_expression,
        ".",
        //FIXME: not all statements are allowed in statement_block
        repeat($._statement),
        kw("endif"),
        "."
      ),

    check_statement: $ => seq(kw("check"), $._logical_expression, "."),

    _logical_expression: $ =>
      choice(
        $.comparison_expression,
        prec.right(4, seq(kw("not"), $._logical_expression)),
        prec.left(
          1,
          seq($._logical_expression, kw("or"), $._logical_expression)
        ),
        prec.left(
          2,
          seq($._logical_expression, kw("and"), $._logical_expression)
        ),
        prec.left(5, seq($._operand, kw("is"), kw("initial")))
      ),

    comparison_expression: $ =>
      seq(
        $._general_expression_position,
        choice("=", kw("eq"), "<>", kw("ne")),
        $._general_expression_position
      ),

    _general_expression_position: $ =>
      choice(
        $.numeric_literal,
        $.character_literal,
        $._data_object,
        $._calculation_expression
      ),

    _calculation_expression: $ => choice($.arithmetic_expression),

    arithmetic_expression: $ =>
      choice(
        prec.left(
          1,
          seq(
            $._general_expression_position,
            "+",
            $._general_expression_position
          )
        ),
        prec.left(
          1,
          seq(
            $._general_expression_position,
            "-",
            $._general_expression_position
          )
        ),
        prec.left(
          2,
          seq(
            $._general_expression_position,
            "*",
            $._general_expression_position
          )
        ),
        prec.left(
          2,
          seq(
            $._general_expression_position,
            "/",
            $._general_expression_position
          )
        ),
        prec.left(
          2,
          seq(
            $._general_expression_position,
            "DIV",
            $._general_expression_position
          )
        ),
        prec.left(
          2,
          seq(
            $._general_expression_position,
            "MOD",
            $._general_expression_position
          )
        ),
        prec.left(
          3,
          seq(
            $._general_expression_position,
            "**",
            $._general_expression_position
          )
        )
      ),

    _writeable_expression: $ =>
      choice(
        // inline declaration
        // constructor expression
        $.table_expression
      ),

    table_expression: $ =>
      seq(
        field("itab", $.name),
        token.immediate("[ "),
        //"[",
        field(
          "line_spec",
          choice(
            $._general_expression_position,
            alias($._table_expression_free_key, $.free_key)
            //alias($._table_expression_table_key, $.table_key)
          )
        ),
        //token.immediate(" ]")
        "]"
      ),

    _table_expression_free_key: $ => repeat1($.comp_spec),

    comp_spec: $ =>
      seq(
        field("component", $.name),
        "=",
        field("operand", $._general_expression_position)
      ),

    select_statement_obsolete: $ =>
      seq(
        kw("select"),
        $.select_list,
        optional(
          seq(kw("up"), kw("to"), $._general_expression_position, kw("rows"))
        ),
        kw("from"),
        alias($.name, $.data_source),
        alias($._select_target, $.target),
        optional(
          seq(optional($.for_all_entries), alias($._where_clause, $.where))
        ),
        "."
      ),

    select_list: $ => choice("*"),

    _select_target: $ =>
      choice(
        seq(kw("into"), " ( ", $.name, repeat(seq(",", $.name)), " ) "),
        seq(
          kw("into"),
          optional(seq(kw("corresponding"), kw("fields"), kw("of"))),
          $.name
        ),
        seq(
          choice(kw("into"), kw("appending")),
          optional(seq(kw("corresponding"), kw("fields"), kw("of"))),
          kw("table"),
          $.name
        )
      ),

    for_all_entries: $ =>
      seq(kw("for"), kw("all"), kw("entries"), kw("in"), $.name),

    _where_clause: $ => seq(kw("where"), $._sql_condition),

    _sql_condition: $ => $._logical_expression,

    read_table_statement: $ =>
      seq(
        kw("read"),
        kw("table"),
        $.name,
        choice(
          seq($.line_spec, $._read_table_result),
          seq($._read_table_result, $.line_spec)
        ),
        "."
      ),

    line_spec: $ =>
      seq(
        kw("with"),
        kw("key"),
        repeat1(seq($.name, "=", $._general_expression_position)),
        optional(seq(kw("binary"), kw("search")))
      ),

    _read_table_result: $ =>
      choice(
        seq(kw("into"), $.name),
        seq(kw("transporting"), kw("no"), kw("fields"))
      ),

    _data_object: $ =>
      choice(
        $.name,
        $.field_symbol_name,
        $.structured_data_object,
        $.attribute_access_static
      ),

    structured_data_object: $ =>
      seq(
        alias(choice($.name, $.field_symbol_name), $.structure_name),
        repeat1(seq(token.immediate("-"), alias($.name, $.component_name)))
      ),

    attribute_access_static: $ =>
      seq(
        field("class", $.name),
        token.immediate("=>"),
        field("attribute", $.name)
      ),

    assignment: $ =>
      seq(
        choice($._data_object, $._writeable_expression),
        "=",
        $._general_expression_position,
        "."
      ),

    try_catch_statement: $ =>
      seq(
        kw("try"),
        ".",
        optional($.try_block),
        repeat($.catch_statement),
        kw("endtry"),
        "."
      ),

    try_block: $ => repeat1($._statement),

    catch_statement: $ =>
      seq(
        kw("catch"),
        field("exception", $.name),
        optional(seq(kw("into"), field("oref", $.name))),
        ".",
        optional($.catch_block)
      ),

    catch_block: $ => repeat1($._statement),

    write_statement: $ =>
      seq(kw("write"), optional("/"), $._general_expression_position, "."),

    chained_write_statement: $ =>
      seq(
        kw("write"),
        ":",
        optional("/"),
        repeat1(
          choice(
            $._general_expression_position,
            seq(",", $._general_expression_position)
          )
        ),
        "."
      ),

    call_method: $ =>
      seq(
        field("name", $.name),
        token.immediate("("),
        field(
          "parameters",
          optional(
            choice(
              $._general_expression_position,
              $.parameter_list,
              $._explicit_parameter_list
            )
          )
        ),
        ")",
        "."
      ),

    parameter_list: $ => repeat1($.parameter_binding),

    _explicit_parameter_list: $ =>
      seq(
        repeat1(
          choice(
            seq(kw("exporting"), $.parameter_list),
            seq(kw("importing"), $.parameter_list),
            seq(kw("changing"), $.parameter_list),
            seq(kw("receiving"), $.parameter_binding)
          )
        )
      ),

    parameter_list_exporting: $ =>
      repeat1(alias($.parameter_binding_exporting, $.parameter_binding)),

    parameter_binding: $ =>
      seq(
        field("formal_parameter", $.name),
        "=",
        field("actual_parameter", $._general_expression_position)
      ),

    parameter_binding_exporting: $ =>
      seq(
        field("formal_parameter", $.name),
        "=",
        field("actual_parameter", $.name)
      ),

    call_method_static: $ =>
      seq(
        field("class_name", $.name),
        token.immediate("=>"),
        field("method_name", $.name),
        token.immediate("("),
        field(
          "parameters",
          optional(
            choice(
              $._general_expression_position,
              $.parameter_list,
              $._explicit_parameter_list
            )
          )
        ),
        ")",
        "."
      ),

    call_method_instance: $ =>
      seq(
        field("instance_name", $.name),
        token.immediate("->"),
        field("method_name", $.name),
        token.immediate("("),
        field(
          "parameters",
          optional(
            choice(
              $._general_expression_position,
              $.parameter_list,
              $._explicit_parameter_list
            )
          )
        ),
        ")",
        "."
      ),

    call_function: $ =>
      seq(
        kw("call"),
        kw("function"),
        field("name", $.character_literal),
        field("parameters", optional($._function_parameter_list)),
        field("exceptions", optional($.exception_list)),
        "."
      ),

    _function_parameter_list: $ =>
      repeat1(
        choice(
          seq(
            kw("exporting"),
            alias($.parameter_list_exporting, $.parameter_list)
          ),
          seq(kw("importing"), $.parameter_list),
          seq(kw("changing"), $.parameter_list)
        )
      ),

    exception_list: $ => seq(kw("exceptions"), repeat1($.return_code_binding)),

    return_code_binding: $ =>
      seq(
        field("exception", $.name),
        "=",
        field("return_code", $.numeric_literal)
      ),

    raise_exception_statement: $ =>
      seq(
        kw("raise"),
        kw("exception"),
        choice(
          seq(
            kw("type"),
            field("class", $.name),
            optional(
              field("parameters", seq(kw("exporting"), $.parameter_list))
            )
          ),
          field("oref", $.name)
        ),
        "."
      ),

    clear_statement: $ => seq(kw("clear"), $._data_object, "."),

    append_statement: $ =>
      seq(
        kw("append"),
        field("line_spec", $.name),
        kw("to"),
        field("itab", $.name),
        "."
      ),

    append_statement_obsolete: $ =>
      seq(kw("append"), field("itab", $.name), "."),

    create_object_statement: $ =>
      seq(
        kw("create"),
        kw("object"),
        $.name,
        optional(seq(kw("exporting"), field("parameters", $.parameter_list))),
        "."
      ),

    include_statement: $ =>
      seq(
        kw("include"),
        choice($.name, $.field_symbol_name),
        optional(seq(kw("if"), kw("found"))),
        "."
      ),

    macro_include: $ =>
      seq(
        field("name", $.name),
        optional(
          alias(repeat1($._general_expression_position), $.parameter_list)
        ),
        "."
      ),

    //_marco_parameter_list: $ => repeat1($._general_expression_position),

    function_implementation: $ =>
      seq(
        kw("function"),
        field("name", $.name),
        ".",
        repeat($._implementation_statement),
        kw("endfunction"),
        "."
      ),

    raise_statement: $ => seq(kw("raise"), $.name, "."),

    _operand: $ => choice($._escaped_operand, $.name),

    _escaped_operand: $ => seq("!", $.name),

    numeric_literal: $ => /[0-9]+/,

    character_literal: $ => /'[^']+'/,

    eol_comment: $ => seq('"', /[^\n]*/),

    bol_comment: $ => seq("*", /[^\n]*/),

    name: $ => /[a-zA-Z_][a-zA-Z0-9_]{0,29}/i,

    field_symbol_name: $ => /<[a-zA-Z0-9_]{0,28}>/i,
  },
});

/**
 * ABAP word/keyword
 * @param {string} word ABAP word as string
 */
function kw(word) {
  return alias(new RegExp(word, "i"), word);
}
