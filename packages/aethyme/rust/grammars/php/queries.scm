; === PHP Symbol + Import Queries ===

; --- Functions ---
(function_definition
  name: (name) @symbol.function.name
) @symbol.function

(method_declaration
  name: (name) @symbol.function.name
) @symbol.function

; --- Classes / Interfaces / Traits ---
(class_declaration
  name: (name) @symbol.class.name
) @symbol.class

(interface_declaration
  name: (name) @symbol.class.name
) @symbol.class

(trait_declaration
  name: (name) @symbol.class.name
) @symbol.class

; --- Imports (use statements) ---
; Captures `use Foo\Bar\Baz;` → "Foo\Bar\Baz" via qualified_name
(namespace_use_declaration
  (namespace_use_clause
    (qualified_name) @import.source
  )
)

; Captures `use Baz;` → "Baz" via simple name
(namespace_use_declaration
  (namespace_use_clause
    (name) @import.source
  )
)
