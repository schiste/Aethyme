; === TypeScript Symbol + Import Queries ===

; --- Functions ---
(function_declaration
  name: (identifier) @symbol.function.name
) @symbol.function

(method_definition
  name: (property_identifier) @symbol.function.name
) @symbol.function

; --- Classes ---
(class_declaration
  name: (type_identifier) @symbol.class.name
) @symbol.class

(interface_declaration
  name: (type_identifier) @symbol.class.name
) @symbol.class

(type_alias_declaration
  name: (type_identifier) @symbol.class.name
) @symbol.class

; --- Constants ---
(lexical_declaration
  (variable_declarator
    name: (identifier) @symbol.constant.name
  )
) @symbol.constant

; --- Imports ---
(import_statement
  source: (string
    (string_fragment) @import.source
  )
)
