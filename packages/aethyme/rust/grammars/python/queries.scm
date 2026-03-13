; === Python Symbol + Import Queries ===

; --- Functions ---
(function_definition
  name: (identifier) @symbol.function.name
) @symbol.function

; --- Classes ---
(class_definition
  name: (identifier) @symbol.class.name
) @symbol.class

; --- Imports ---
(import_from_statement
  module_name: (dotted_name) @import.source
)

(import_statement
  name: (dotted_name) @import.source
)
