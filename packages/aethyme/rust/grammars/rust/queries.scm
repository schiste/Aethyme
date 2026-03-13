; === Rust Symbol + Import Queries ===

; --- Functions ---
(function_item
  name: (identifier) @symbol.function.name
) @symbol.function

; --- Structs / Enums / Traits ---
(struct_item
  name: (type_identifier) @symbol.class.name
) @symbol.class

(enum_item
  name: (type_identifier) @symbol.class.name
) @symbol.class

(trait_item
  name: (type_identifier) @symbol.class.name
) @symbol.class

; --- Constants ---
(const_item
  name: (identifier) @symbol.constant.name
) @symbol.constant

; --- Imports ---
(use_declaration
  argument: (_) @import.source
)

(mod_item
  name: (identifier) @import.source
)
