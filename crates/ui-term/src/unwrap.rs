//! Text a program laid out to the terminal's width (Claude Code breaks its
//! paragraphs and indents them), as paragraphs again: to paste it where the
//! width is another.

/// A selected row of the terminal.
pub struct Row {
    /// What's selected of it.
    pub text: String,
    /// The column its text ends at, selected or not.
    pub end: usize,
    /// The terminal wrapped it into the next one.
    pub wraps: bool,
}

/// The rows without the indentation they share and without the breaks put
/// to fit `cols` columns. A row was broken to fit if the next one's first
/// word wouldn't have fitted after it; and a word broken in the middle (a
/// long path or URL) goes on without a space.
pub fn unwrap(rows: &[Row], cols: usize) -> String {
    // The rows the terminal wrapped, joined first.
    let mut lines: Vec<(String, usize)> = Vec::new();
    let mut joining = false;
    for row in rows {
        match lines.last_mut() {
            Some((text, end)) if joining => {
                text.push_str(&row.text);
                *end = row.end;
            }
            _ => lines.push((row.text.clone(), row.end)),
        }
        joining = row.wraps;
    }
    let Some((first, _)) = lines.first_mut() else {
        return String::new();
    };
    if let Some(rest) = first.strip_prefix("⏺ ").or_else(|| first.strip_prefix("● ")) {
        *first = format!("  {rest}");
    }
    let indent = lines
        .iter()
        .skip(1)
        .filter(|(text, _)| !text.trim().is_empty())
        .map(|(text, _)| leading(text))
        .min()
        .unwrap_or_else(|| leading(&lines[0].0));

    let mut out = String::new();
    for (ix, (text, _)) in lines.iter().enumerate() {
        let text = &text[text.char_indices().nth(leading(text).min(indent)).map_or(text.len(), |(at, _)| at)..];
        if ix == 0 {
            out.push_str(text);
            continue;
        }
        match separator(&lines[ix - 1], &lines[ix], cols) {
            Some(separator) => {
                out.push_str(separator);
                out.push_str(text.trim_start());
            }
            None => {
                out.push('\n');
                out.push_str(text);
            }
        }
    }
    out
}

/// What goes between a line and the next if the next only went down to fit:
/// a space, or nothing in the middle of a word.
fn separator((previous, end): &(String, usize), (next, next_end): &(String, usize), cols: usize) -> Option<&'static str> {
    let last_word = previous.split_whitespace().last()?;
    let next_word = next.split_whitespace().next()?;
    if starts_block(next.trim_start()) {
        return None;
    }
    let first = next_word.chars().count();
    let room = cols.saturating_sub(leading(next));
    // Broken in the middle: the whole word didn't fit in a row by itself.
    if *end >= cols && last_word.chars().count() + first > room {
        return Some("");
    }
    // A row that is a piece of a word too long for any row: it went down
    // because it's long, not because the line goes on.
    let piece = *next_end >= cols && next.trim().chars().count() == first;
    (end + 1 + first > cols && !piece).then_some(" ")
}

/// Lines that start something of their own: a list item, a heading, a
/// quote, a table, a code fence, another of Claude Code's messages.
fn starts_block(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") ")) {
        return true;
    }
    ["- ", "* ", "+ ", "• ", "#", ">", "|", "│", "```", "⏺", "⎿", "❯"]
        .iter()
        .any(|start| line.starts_with(start))
}

fn leading(text: &str) -> usize {
    text.chars().take_while(|c| *c == ' ').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows as Claude Code leaves them, all selected.
    fn rows(text: &str) -> Vec<Row> {
        text.lines()
            .map(|line| Row {
                text: line.to_string(),
                end: line.trim_end().chars().count(),
                wraps: false,
            })
            .collect()
    }

    #[test]
    fn paragraphs_and_list_items_come_back_whole() {
        let text = "\
⏺ Instalado. Cierra y abre Den y vuelve a probar con las dos de arriba. Había dos fallos:

  1. El clic derecho no ofrecía nada: el menú del panel se quedaba con el clic antes de que el
     terminal mirase qué había debajo. Ahora el terminal lo mira antes. Mi test anterior no tenía
     ese menú alrededor, por eso pasaba; el nuevo sí lo tiene, y he comprobado que falla sin el
     arreglo.
  2. La ruta y la URL partidas: he leído tu terminal y mide 76 columnas. Las dos filas llegan
     justo al borde, pero Claude Code sangra la continuación con 2 espacios y yo esperaba texto
     en la columna\u{a0}0. Ahora la continuación se busca después de la sangría, y el subrayado marca
     solo las letras de la ruta, sin esos espacios.

  Los tests pasan y he probado los fallos en tests, pero en la app aún no.";
        assert_eq!(
            unwrap(&rows(text), 97),
            "\
Instalado. Cierra y abre Den y vuelve a probar con las dos de arriba. Había dos fallos:

1. El clic derecho no ofrecía nada: el menú del panel se quedaba con el clic antes de que el terminal mirase qué había debajo. Ahora el terminal lo mira antes. Mi test anterior no tenía ese menú alrededor, por eso pasaba; el nuevo sí lo tiene, y he comprobado que falla sin el arreglo.
2. La ruta y la URL partidas: he leído tu terminal y mide 76 columnas. Las dos filas llegan justo al borde, pero Claude Code sangra la continuación con 2 espacios y yo esperaba texto en la columna\u{a0}0. Ahora la continuación se busca después de la sangría, y el subrayado marca solo las letras de la ruta, sin esos espacios.

Los tests pasan y he probado los fallos en tests, pero en la app aún no."
        );
    }

    /// Short lines are lines of their own; a path broken at the edge goes
    /// on without a space; rows the terminal wrapped stay together.
    #[test]
    fn short_lines_stay_and_broken_words_join() {
        let text = "\
  one
  two
  /Users/me/a/very/long/
  path.txt";
        assert_eq!(unwrap(&rows(text), 24), "one\ntwo\n/Users/me/a/very/long/path.txt");

        let wrapped = vec![
            Row { text: "  abc".into(), end: 5, wraps: true },
            Row { text: "def".into(), end: 3, wraps: false },
        ];
        assert_eq!(unwrap(&wrapped, 5), "abcdef");
    }
}
