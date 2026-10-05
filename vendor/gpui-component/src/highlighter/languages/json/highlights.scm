(comment) @comment

; (den) As VS Code: keys and string values each their color. The first
; capture of a node wins, so these go before `(string)`.
(pair key: (string) @string.key)
(pair value: (string) @string.value)
(array (string) @string.value)

(string) @string
(escape_sequence) @string.escape

(number) @number

[
  (true)
  (false)
] @boolean

(null) @constant.builtin

[
  ","
  ":"
  "{"
  "}"
  "["
  "]"
] @punctuation
