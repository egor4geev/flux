; Копия queries/highlights-params.scm из tree-sitter-javascript 0.25.0 (MIT,
; Copyright (c) 2014 Max Brunsfeld). Rust-биндинг крейта не экспортирует этот файл,
; а tree-sitter.json грамматики подключает его к подсветке JavaScript.

(formal_parameters
  [
    (identifier) @variable.parameter
    (array_pattern
      (identifier) @variable.parameter)
    (object_pattern
      [
        (pair_pattern value: (identifier) @variable.parameter)
        (shorthand_property_identifier_pattern) @variable.parameter
      ])
  ]
)
