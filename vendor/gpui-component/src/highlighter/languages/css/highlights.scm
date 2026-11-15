; (den) CSS colored like VS Code: selectors (with their `.`, `#` and `:`),
; property names, values, numbers with their units, strings. The first
; pattern that matches a node wins, so the specific ones go first.

(comment) @comment

; Custom properties and var(--x)
((property_name) @string.key
 (#match? @string.key "^--"))
((plain_value) @string.key
 (#match? @string.key "^--"))

; Selectors
(class_selector "." @tag)
(class_name) @tag
(id_selector "#" @tag)
(id_name) @tag
(tag_name) @tag
(nesting_selector) @tag
(universal_selector) @tag
(pseudo_class_selector ":" @tag)
(pseudo_element_selector "::" @tag)
(namespace_name) @tag
(attribute_name) @attribute
(attribute_selector (plain_value) @string.value)

; Declarations
(property_name) @attribute
(feature_name) @attribute
(function_name) @function
(important) @keyword
(string_value) @string.value
(color_value) @attribute.value
(integer_value) @number
(float_value) @number
(unit) @number
(plain_value) @attribute.value

; At-rules
"@media" @keyword
"@import" @keyword
"@charset" @keyword
"@namespace" @keyword
"@supports" @keyword
"@keyframes" @keyword
(at_keyword) @keyword
(keyframes_name) @function
(to) @keyword
(from) @keyword
"and" @keyword
"or" @keyword
"not" @keyword
"only" @keyword

"~" @operator
">" @operator
"+" @operator
"-" @operator
"*" @operator
"/" @operator
"=" @operator
"^=" @operator
"|=" @operator
"~=" @operator
"$=" @operator
"*=" @operator

"," @punctuation.delimiter
":" @punctuation.delimiter
";" @punctuation.delimiter
