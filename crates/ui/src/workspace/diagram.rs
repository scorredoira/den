//! Mermaid diagrams in Markdown previews: a ```mermaid block becomes an SVG
//! image. Only `stateDiagram`/`stateDiagram-v2` and `flowchart`/`graph`, with
//! no subgraphs, composite states or styles, laid out in layers (Sugiyama):
//! ranks, crossings reduced by barycenters, positions by isotonic regression.
//! The image is centered; a block it can't draw stays as code under the
//! reason.
use std::{borrow::Cow, collections::HashMap, fmt::Write as _};

/// `markdown` with each Mermaid block replaced by its image, its text drawn
/// at `font_size` like the rest of the preview.
pub(super) fn with_diagrams(markdown: &str, font_size: f32) -> Cow<'_, str> {
    if !markdown.contains("mermaid") {
        return Cow::Borrowed(markdown);
    }
    let mut out = String::with_capacity(markdown.len());
    let mut lines = markdown.split_inclusive('\n');
    let mut changed = false;
    while let Some(line) = lines.next() {
        let Some(fence) = mermaid_fence(line) else {
            out.push_str(line);
            continue;
        };
        let mut block = String::new();
        let mut raw = line.to_string();
        for line in lines.by_ref() {
            raw.push_str(line);
            if closes(line, fence) {
                break;
            }
            block.push_str(line);
        }
        changed = true;
        match rendered(&block) {
            Ok(svg) => {
                let _ = write!(
                    out,
                    "\n<p align=\"center\"><img src=\"data:image/svg+xml,{}\" width=\"{}\" style=\"max-width: none\" alt=\"diagram\"></p>\n\n",
                    percent_encode(&svg.text),
                    (svg.width * font_size / FONT).ceil(),
                );
            }
            Err(error) => {
                let _ = write!(out, "\n> Diagram not drawn: {error}\n\n{raw}");
                if !raw.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    if changed { Cow::Owned(out) } else { Cow::Borrowed(markdown) }
}

/// `render`, remembered by source: the preview is made again on every edit,
/// and a big diagram takes a while to lay out.
fn rendered(source: &str) -> Result<Svg, String> {
    thread_local! {
        static DRAWN: std::cell::RefCell<HashMap<String, Result<Svg, String>>> = Default::default();
    }
    DRAWN.with_borrow_mut(|drawn| {
        if let Some(svg) = drawn.get(source) {
            return svg.clone();
        }
        if drawn.len() >= 64 {
            drawn.clear();
        }
        let svg = render(source);
        drawn.insert(source.to_string(), svg.clone());
        svg
    })
}

/// The opening fence of a Mermaid block: its character and length.
fn mermaid_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let mark = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = trimmed.chars().take_while(|c| *c == mark).count();
    (length >= 3 && trimmed[length..].trim() == "mermaid").then_some((mark, length))
}

fn closes(line: &str, (mark, length): (char, usize)) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= length && trimmed.chars().all(|c| c == mark)
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3 / 2);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/:=,;()!*".contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

// The diagram, parsed.

#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    Rect,
    Round,
    Diamond,
    Start,
    End,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Direction {
    Down,
    Up,
    Right,
    Left,
}

#[derive(Debug)]
struct Node {
    label: Vec<String>,
    shape: Shape,
}

#[derive(Debug)]
struct Edge {
    from: usize,
    to: usize,
    label: Vec<String>,
    dashed: bool,
    arrow: bool,
}

#[derive(Debug)]
struct Diagram {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    direction: Direction,
}

#[derive(Default)]
struct Builder {
    ids: HashMap<String, usize>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Builder {
    fn node(&mut self, id: &str, shape: Shape) -> usize {
        if let Some(&ix) = self.ids.get(id) {
            return ix;
        }
        self.nodes.push(Node { label: vec![id.to_string()], shape });
        self.ids.insert(id.to_string(), self.nodes.len() - 1);
        self.nodes.len() - 1
    }
}

fn parse(source: &str) -> Result<Diagram, String> {
    let mut d = parse_lines(source)?;
    for node in &mut d.nodes {
        node.label = wrap(&node.label);
    }
    let mut edges: Vec<Edge> = Vec::new();
    for mut edge in d.edges {
        edge.label = wrap(&edge.label);
        // Arrows between the same two boxes are drawn as one, their labels
        // stacked with a rule between them.
        match edges.iter_mut().find(|e| (e.from, e.to, e.dashed, e.arrow) == (edge.from, edge.to, edge.dashed, edge.arrow)) {
            Some(same) if !edge.label.is_empty() => {
                if !same.label.is_empty() {
                    same.label.push(String::new());
                }
                same.label.append(&mut edge.label);
            }
            Some(_) => {}
            None => edges.push(edge),
        }
    }
    d.edges = edges;
    Ok(d)
}

fn parse_lines(source: &str) -> Result<Diagram, String> {
    let mut lines = source.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("%%"));
    let header = lines.next().ok_or("the block is empty")?;
    let mut words = header.split_whitespace();
    match words.next() {
        Some("stateDiagram" | "stateDiagram-v2") => parse_states(lines),
        Some("flowchart" | "graph") => {
            let direction = match words.next() {
                None | Some("TD" | "TB") => Direction::Down,
                Some("BT") => Direction::Up,
                Some("LR") => Direction::Right,
                Some("RL") => Direction::Left,
                Some(other) => return Err(format!("unknown direction `{other}`")),
            };
            parse_flowchart(lines, direction)
        }
        _ => Err(format!("`{header}`: only stateDiagram and flowchart are drawn")),
    }
}

fn direction_line(line: &str) -> Option<Result<Direction, String>> {
    let rest = line.strip_prefix("direction ")?;
    Some(match rest.trim() {
        "TB" | "TD" => Ok(Direction::Down),
        "BT" => Ok(Direction::Up),
        "LR" => Ok(Direction::Right),
        "RL" => Ok(Direction::Left),
        other => Err(format!("unknown direction `{other}`")),
    })
}

fn parse_states<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Diagram, String> {
    let mut b = Builder::default();
    let mut direction = Direction::Down;
    for line in lines {
        if let Some(d) = direction_line(line) {
            direction = d?;
            continue;
        }
        if line.ends_with('{') || line.starts_with("note ") || line.starts_with("classDef ") {
            return Err(format!("`{line}`: composite states, notes and styles are not drawn"));
        }
        if let Some(rest) = line.strip_prefix("state ") {
            let rest = rest.trim();
            let (label, id) = rest
                .strip_prefix('"')
                .and_then(|r| r.split_once('"'))
                .and_then(|(label, r)| Some((label, r.trim().strip_prefix("as ")?.trim())))
                .ok_or_else(|| format!("`{line}`: expected `state \"Label\" as Id`"))?;
            let ix = b.node(state_id(id, line)?, Shape::Round);
            b.nodes[ix].label = lines_of(label);
            continue;
        }
        let (body, label) = match line.split_once(':') {
            Some((body, label)) => (body.trim(), lines_of(label.trim())),
            None => (line, Vec::new()),
        };
        match body.split_once("-->") {
            Some((from, to)) => {
                let (from, to) = (from.trim(), to.trim());
                let from = match from {
                    "[*]" => b.node("[*] start", Shape::Start),
                    id => b.node(state_id(id, line)?, Shape::Round),
                };
                let to = match to {
                    "[*]" => b.node("[*] end", Shape::End),
                    id => b.node(state_id(id, line)?, Shape::Round),
                };
                b.edges.push(Edge { from, to, label, dashed: false, arrow: true });
            }
            None if !label.is_empty() => {
                let ix = b.node(state_id(body, line)?, Shape::Round);
                b.nodes[ix].label = label;
            }
            None => {
                b.node(state_id(body, line)?, Shape::Round);
            }
        }
    }
    Ok(Diagram { nodes: b.nodes, edges: b.edges, direction })
}

fn state_id<'a>(id: &'a str, line: &str) -> Result<&'a str, String> {
    if id.is_empty() || id.contains(char::is_whitespace) || id.contains(['[', ']', '"', '{', '}']) {
        return Err(format!("`{line}`: `{id}` is not a state name"));
    }
    Ok(id)
}

/// A label's lines: Mermaid breaks them with `<br>`.
fn lines_of(label: &str) -> Vec<String> {
    let label = label.trim();
    let label = label.strip_prefix('"').and_then(|l| l.strip_suffix('"')).unwrap_or(label);
    label
        .replace("<br/>", "<br>")
        .replace("<br />", "<br>")
        .split("<br>")
        .map(|line| line.trim().to_string())
        .collect()
}

fn parse_flowchart<'a>(lines: impl Iterator<Item = &'a str>, mut direction: Direction) -> Result<Diagram, String> {
    let mut b = Builder::default();
    for line in lines {
        if let Some(d) = direction_line(line) {
            direction = d?;
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        if ["subgraph", "end", "classDef", "class", "style", "linkStyle", "click"].contains(&first) {
            return Err(format!("`{line}`: subgraphs and styles are not drawn"));
        }
        let mut p = Cursor { text: line.trim_end_matches(';'), at: 0 };
        let mut from = flow_nodes(&mut p, &mut b, line)?;
        loop {
            p.skip_spaces();
            if p.done() {
                break;
            }
            let (dashed, arrow, label) = flow_link(&mut p, line)?;
            p.skip_spaces();
            let to = flow_nodes(&mut p, &mut b, line)?;
            for &f in &from {
                for &t in &to {
                    b.edges.push(Edge { from: f, to: t, label: label.clone(), dashed, arrow });
                }
            }
            from = to;
        }
    }
    Ok(Diagram { nodes: b.nodes, edges: b.edges, direction })
}

struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl Cursor<'_> {
    fn rest(&self) -> &str {
        &self.text[self.at..]
    }
    fn done(&self) -> bool {
        self.rest().trim().is_empty()
    }
    fn skip_spaces(&mut self) {
        self.at = self.text.len() - self.rest().trim_start().len();
    }
}

/// One node or several joined by `&` (`A & B --> C`): a link from or to
/// them is one edge for each.
fn flow_nodes(p: &mut Cursor, b: &mut Builder, line: &str) -> Result<Vec<usize>, String> {
    let mut nodes = vec![flow_node(p, b, line)?];
    loop {
        p.skip_spaces();
        let Some(rest) = p.rest().strip_prefix('&') else {
            return Ok(nodes);
        };
        p.at = p.text.len() - rest.len();
        nodes.push(flow_node(p, b, line)?);
    }
}

/// `id`, `id[text]`, `id(text)`, `id([text])`, `id((text))` or `id{text}`.
fn flow_node(p: &mut Cursor, b: &mut Builder, line: &str) -> Result<usize, String> {
    p.skip_spaces();
    let id_len = p.rest().find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(p.rest().len());
    if id_len == 0 {
        return Err(format!("`{line}`: expected a node at `{}`", p.rest()));
    }
    let id = p.rest()[..id_len].to_string();
    p.at += id_len;
    let shapes = [("((", "))", Shape::Round), ("([", "])", Shape::Round), ("[", "]", Shape::Rect), ("(", ")", Shape::Round), ("{", "}", Shape::Diamond)];
    let Some(&(open, close, shape)) = shapes.iter().find(|(open, ..)| p.rest().starts_with(open)) else {
        return Ok(b.node(&id, Shape::Rect));
    };
    let inner = &p.rest()[open.len()..];
    let end = inner.find(close).ok_or_else(|| format!("`{line}`: `{open}` with no `{close}`"))?;
    let label = lines_of(&inner[..end]);
    p.at += open.len() + end + close.len();
    let ix = b.node(&id, shape);
    b.nodes[ix].shape = shape;
    b.nodes[ix].label = label;
    Ok(ix)
}

/// `-->`, `---`, `-.->`, `==>` and the like, with `|text|` after it or the
/// text inside (`-- text -->`): dashed, arrow, label.
fn flow_link(p: &mut Cursor, line: &str) -> Result<(bool, bool, Vec<String>), String> {
    let run = p.rest().find(|c| !matches!(c, '-' | '.' | '=')).unwrap_or(p.rest().len());
    let mut token = p.rest()[..run].to_string();
    p.at += run;
    let mut label = Vec::new();
    if matches!(token.as_str(), "--" | "==" | "-.") && p.rest().starts_with(' ') {
        let rest = p.rest();
        let end = ["-->", "---", ".->", ".-", "==>", "==="]
            .iter()
            .filter_map(|close| rest.find(close))
            .min()
            .ok_or_else(|| format!("`{line}`: a link's text with no end"))?;
        label = lines_of(&rest[..end]);
        p.at += end;
        let run = p.rest().find(|c| !matches!(c, '-' | '.' | '=')).unwrap_or(p.rest().len());
        token.push_str(&p.rest()[..run]);
        p.at += run;
    }
    if token.len() < 2 {
        return Err(format!("`{line}`: expected a link at `{}{}`", token, p.rest()));
    }
    let arrow = p.rest().starts_with('>');
    if arrow {
        p.at += 1;
    } else if p.rest().starts_with(['o', 'x']) && p.rest()[1..].starts_with(' ') {
        return Err(format!("`{line}`: only `>` arrowheads are drawn"));
    }
    if label.is_empty() && p.rest().starts_with('|') {
        let end = p.rest()[1..].find('|').ok_or_else(|| format!("`{line}`: `|` with no closing `|`"))?;
        label = lines_of(&p.rest()[1..1 + end]);
        p.at += end + 2;
    }
    Ok((token.contains('.'), arrow, label))
}

// Sizes: text measured by an average width per kind of letter, in the
// fonts of the SVG (Helvetica, Arial).

const FONT: f32 = 14.0;
const LINE: f32 = 18.0;

fn text_width(line: &str) -> f32 {
    line.chars()
        .map(|c| match c {
            'i' | 'l' | 'j' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' | 'í' | '¡' => 0.28,
            'f' | 't' | 'r' | 'I' | '(' | ')' | '[' | ']' | '/' | ' ' | '-' => 0.36,
            'm' | 'w' | 'M' | 'W' => 0.86,
            'A'..='Z' | 'Á' | 'É' | 'Ó' | 'Ú' | 'Ñ' => 0.68,
            _ => 0.56,
        })
        .sum::<f32>()
        * FONT
}

/// Text wider than this breaks between words, as Mermaid does.
const WRAP: f32 = 190.0;
/// The height of the rule between stacked labels, an empty line.
const RULE: f32 = 9.0;

/// Each line broken into as few lines as fit in `WRAP`, as even as they can
/// be, so the last one is not a lone word.
fn wrap(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let count = break_at(line, WRAP).len();
        let mut limit = text_width(line) / count as f32;
        let mut broken = break_at(line, limit);
        while broken.len() > count {
            limit += 4.0;
            broken = break_at(line, limit);
        }
        out.append(&mut broken);
    }
    out
}

fn break_at(line: &str, limit: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        if !current.is_empty() && text_width(&format!("{current} {word}")) > limit {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    out.push(current);
    out
}

fn line_height(line: &str) -> f32 {
    if line.is_empty() { RULE } else { LINE }
}

fn text_size(lines: &[String]) -> (f32, f32) {
    let width = lines.iter().map(|l| text_width(l)).fold(0.0, f32::max);
    (width, lines.iter().map(|l| line_height(l)).sum())
}

fn node_size(node: &Node) -> (f32, f32) {
    let (w, h) = text_size(&node.label);
    match node.shape {
        Shape::Start => (16.0, 16.0),
        Shape::End => (20.0, 20.0),
        Shape::Rect | Shape::Round => ((w + 28.0).max(56.0), h + 20.0),
        // The text's box fits a rhombus whose half-diagonals take 62% of its
        // width and 38% of its height.
        Shape::Diamond => ((w + 10.0) / 0.62, (h + 6.0) / 0.38),
    }
}

// Layout.

const RANK_GAP: f32 = 36.0;
const NODE_GAP: f32 = 48.0;
const DUMMY_GAP: f32 = 14.0;
const LOOP: f32 = 34.0;
const MARGIN: f32 = 12.0;
/// Around a label, in the room kept for it.
const LABEL_GAP: f32 = 14.0;
/// Edges meeting a box's side keep this far from its corners and at most
/// this far from each other.
const PORT_INSET: f32 = 10.0;
const PORT_GAP: f32 = 12.0;
/// Between an edge and a box it passes.
const CLEAR: f32 = 6.0;
/// Between ranks where an edge would run flatter than one in this, the
/// edges go across on tracks, square: down, along, down.
const FLAT: f32 = 3.0;
/// Between a box and the nearest track, and between tracks.
const CHANNEL: f32 = 18.0;
const TRACK_GAP: f32 = 8.0;
/// Of a square edge's corners.
const RADIUS: f32 = 6.0;
/// The largest an SVG is drawn at: its side at twice that fits the GPU's
/// largest texture (16384 pixels), and its area at twice that 128 MB.
const MAX_SIDE: f32 = 8000.0;
const MAX_AREA: f32 = 8_000_000.0;
/// Boxes and edge points beyond which a diagram is ordered from fewer starts.
const BIG: usize = 1000;
/// About a preview's width: a diagram laid out wider has its last states
/// staggered on rows of their own, so it can be drawn at full size.
const COLUMN: f32 = 760.0;

/// A box of the layered graph: a node, or a point an edge goes through.
struct Item {
    node: Option<usize>,
    /// Its extent along the layer and across it.
    breadth: f32,
    depth: f32,
    rank: usize,
    pos: f32,
}

struct Route {
    edge: usize,
    /// Items from the edge's source to its target, as drawn.
    items: Vec<usize>,
    label: Option<usize>,
    /// The line as drawn, in the SVG: one piece, or two when its label
    /// sits on it; the last one ends at the target.
    pieces: Vec<Vec<(f32, f32)>>,
}

struct Layout {
    /// For the tests: where each box went.
    #[cfg(test)]
    items: Vec<Item>,
    routes: Vec<Route>,
    /// Each node's size as drawn: wider than its text when many edges
    /// meet one side.
    boxes: Vec<(f32, f32)>,
    /// Each node's rank, whether no edge leaves it downward, and whether
    /// none comes in.
    ranks: Vec<usize>,
    sinks: Vec<bool>,
    sources: Vec<bool>,
    /// Item centers in the SVG, and the drawing's size.
    centers: Vec<(f32, f32)>,
    width: f32,
    height: f32,
}

/// The diagram laid out, staggered over two to four rows where it would
/// be wider than COLUMN: in each wide rank, the states nothing leaves go
/// down in blocks, from the left, or else those nothing reaches go up.
fn layout(d: &Diagram) -> Layout {
    let n = d.nodes.len();
    let first = arrange(d, &vec![0; n]);
    if first.width <= COLUMN || matches!(d.direction, Direction::Right | Direction::Left) || first.centers.len() > BIG {
        return first;
    }
    let mut rows: HashMap<usize, Vec<usize>> = HashMap::new();
    for v in 0..n {
        rows.entry(first.ranks[v]).or_default().push(v);
    }
    for row in rows.values_mut() {
        row.sort_by(|&a, &b| first.centers[a].0.total_cmp(&first.centers[b].0));
    }
    // Only the rows that make it wide, and in each its last states, or else
    // its first: staggering the ones between tangles their edges.
    rows.retain(|_, row| row.len() > 1 && first.centers[row[row.len() - 1]].0 - first.centers[row[0]].0 > COLUMN / 2.0);
    for row in rows.values_mut() {
        let ends = if row.iter().filter(|&&v| first.sinks[v]).count() > 1 { &first.sinks } else { &first.sources };
        row.retain(|&v| ends[v]);
    }
    rows.retain(|_, row| row.len() > 1);
    if rows.is_empty() {
        return first;
    }
    // The fewest rows that fit, or else that are about as narrow as the most.
    let mut tried = vec![first];
    for tiers in 2..=4 {
        let mut drop = vec![0; n];
        for row in rows.values() {
            for (i, &v) in row.iter().enumerate() {
                drop[v] = i * tiers / row.len();
            }
        }
        tried.push(arrange(d, &drop));
        if tried[tried.len() - 1].width <= COLUMN {
            break;
        }
    }
    let narrowest = tried.iter().map(|l| l.width).fold(f32::MAX, f32::min);
    let at = tried.iter().position(|l| l.width <= COLUMN.max(narrowest * 1.1)).unwrap();
    tried.swap_remove(at)
}

/// The layout with each node in `drop` that many rows further down.
fn arrange(d: &Diagram, drop: &[usize]) -> Layout {
    let horizontal = matches!(d.direction, Direction::Right | Direction::Left);
    let n = d.nodes.len();
    let loops = |v: usize| d.edges.iter().filter(|e| e.from == v && e.to == v).map(|e| LOOP + text_size(&e.label).0).fold(0.0, f32::max);
    let mut items: Vec<Item> = (0..n)
        .map(|v| {
            let (w, h) = node_size(&d.nodes[v]);
            let (breadth, depth) = if horizontal { (h, w) } else { (w, h) };
            Item { node: Some(v), breadth: breadth + 2.0 * loops(v), depth, rank: 0, pos: 0.0 }
        })
        .collect();

    // Cycles broken by turning the edges that close them, found depth first
    // from the nodes nothing reaches, in the order they were written.
    let mut reversed = vec![false; d.edges.len()];
    let mut state = vec![0u8; n];
    let mut roots: Vec<usize> = (0..n).filter(|&v| !d.edges.iter().any(|e| e.to == v && e.from != v)).collect();
    roots.extend(0..n);
    for root in roots {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some(&(v, next)) = stack.last() {
            match (next..d.edges.len()).find(|&e| d.edges[e].from == v && d.edges[e].to != v) {
                Some(e) => {
                    stack.last_mut().unwrap().1 = e + 1;
                    let w = d.edges[e].to;
                    match state[w] {
                        0 => {
                            state[w] = 1;
                            stack.push((w, 0));
                        }
                        1 => reversed[e] = true,
                        _ => {}
                    }
                }
                None => {
                    state[v] = 2;
                    stack.pop();
                }
            }
        }
    }
    let ends = |e: usize| {
        let edge = &d.edges[e];
        if reversed[e] { (edge.to, edge.from) } else { (edge.from, edge.to) }
    };
    let flow: Vec<usize> = (0..d.edges.len()).filter(|&e| d.edges[e].from != d.edges[e].to).collect();

    // Ranks by longest path, two apart so a label has a rank of its own
    // between them; a node only with edges out then moves down to its
    // nearest successor.
    let settle = |rank: &mut [usize]| {
        for _ in 0..n {
            let mut moved = false;
            for &e in &flow {
                let (u, v) = ends(e);
                if rank[v] < rank[u] + 2 {
                    rank[v] = rank[u] + 2;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    };
    let mut rank = vec![0usize; n];
    settle(&mut rank);
    for v in 0..n {
        let ins = flow.iter().any(|&e| ends(e).1 == v);
        let outs: Vec<usize> = flow.iter().filter(|&&e| ends(e).0 == v).map(|&e| rank[ends(e).1]).collect();
        if !ins && !outs.is_empty() {
            rank[v] = outs.iter().min().unwrap() - 2;
        }
    }
    // Staggered: each node `drop` rows down, and what follows it with it;
    // a node with no edges in goes up instead.
    let sources: Vec<bool> = (0..n).map(|v| !flow.iter().any(|&e| ends(e).1 == v)).collect();
    let sinks: Vec<bool> = (0..n).map(|v| !flow.iter().any(|&e| ends(e).0 == v)).collect();
    let lift = 2 * drop.iter().copied().max().unwrap_or(0);
    for v in 0..n {
        rank[v] = if sources[v] { rank[v] + lift - 2 * drop[v] } else { rank[v] + lift + 2 * drop[v] };
    }
    settle(&mut rank);
    let low = rank.iter().copied().min().unwrap_or(0);
    rank.iter_mut().for_each(|r| *r -= low);
    for (v, item) in items.iter_mut().enumerate() {
        item.rank = rank[v];
    }

    // Each edge goes through a point per rank it crosses; its label is the
    // middle one on a label rank, or the one beside a node staggered.
    let mut routes = Vec::new();
    for &e in &flow {
        let (u, v) = ends(e);
        let mut chain = vec![u];
        for r in rank[u] + 1..rank[v] {
            items.push(Item { node: None, breadth: 0.0, depth: 0.0, rank: r, pos: 0.0 });
            chain.push(items.len() - 1);
        }
        chain.push(v);
        let middle = chain.len() / 2;
        let middle = if drop[v] > 0 && !sources[v] {
            chain.len() - 2
        } else if drop[u] > 0 && sources[u] {
            1
        } else if items[chain[middle]].rank % 2 == 1 {
            middle
        } else {
            middle - 1
        };
        let label = (!d.edges[e].label.is_empty()).then(|| {
            let item = &mut items[chain[middle]];
            let (w, h) = label_box(&d.edges[e].label);
            (item.breadth, item.depth) = if horizontal { (h + LABEL_GAP * 2.0, w) } else { (w + LABEL_GAP * 2.0, h) };
            chain[middle]
        });
        if reversed[e] {
            chain.reverse();
        }
        routes.push(Route { edge: e, items: chain, label, pieces: Vec::new() });
    }

    // Order within each rank: first seen, then barycenter sweeps, keeping
    // the order with the fewest crossings.
    let ranks = items.iter().map(|i| i.rank).max().unwrap_or(0) + 1;
    let mut links: Vec<(usize, usize)> = Vec::new();
    for route in &routes {
        let mut chain = route.items.clone();
        if items[chain[0]].rank > items[*chain.last().unwrap()].rank {
            chain.reverse();
        }
        links.extend(chain.windows(2).map(|w| (w[0], w[1])));
    }
    let mut g = Graph { up: vec![Vec::new(); items.len()], down: vec![Vec::new(); items.len()] };
    for &(a, b) in &links {
        g.down[a].push(b);
        g.up[b].push(a);
    }
    // A box with more edges on a side than fit PORT_GAP apart grows to fit
    // them.
    let mut boxes: Vec<(f32, f32)> = d.nodes.iter().map(node_size).collect();
    for v in 0..n {
        if matches!(d.nodes[v].shape, Shape::Rect | Shape::Round) {
            let most = g.up[v].len().max(g.down[v].len());
            let wanted = most.saturating_sub(1) as f32 * PORT_GAP + 2.0 * PORT_INSET;
            let breadth = if horizontal { &mut boxes[v].1 } else { &mut boxes[v].0 };
            if wanted > *breadth {
                items[v].breadth += wanted - *breadth;
                *breadth = wanted;
            }
        }
    }
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); ranks];
    let mut seen = vec![false; items.len()];
    let mut queue: Vec<usize> = (0..n).filter(|&v| g.up[v].is_empty()).collect();
    queue.extend(0..items.len());
    // The order each item is first reached in, following the edges as they
    // were written: between orders with as many crossings, the one closer
    // to it wins, so `yes` written before `no` goes to its left.
    let mut written = vec![0usize; items.len()];
    let mut found = 0;
    let mut at = 0;
    while at < queue.len() {
        let v = queue[at];
        at += 1;
        if seen[v] {
            continue;
        }
        seen[v] = true;
        layers[items[v].rank].push(v);
        written[v] = found;
        found += 1;
        queue.extend(&g.down[v]);
    }
    let mut starts = vec![layers.clone(), layers.iter().map(|l| l.iter().rev().copied().collect()).collect()];
    let mut seed = 0x9e37_79b9u32;
    // A big diagram tries only those two: each shuffle costs as much again.
    let shuffles = if items.len() > BIG { 0 } else { 6 };
    for _ in 0..shuffles {
        let mut shuffled = layers.clone();
        for layer in &mut shuffled {
            for i in (1..layer.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                layer.swap(i, seed as usize % (i + 1));
            }
        }
        starts.push(shuffled);
    }
    let mut best = layers.clone();
    let mut fewest = Score::WORST;
    for start in starts {
        let (ordered, score) = order(start, &g, &written);
        if score < fewest {
            best = ordered;
            fewest = score;
        }
    }
    let layers = best;

    // Positions along the layer: each item toward the mean of its
    // neighbors, kept in order and apart.
    let gap = |a: &Item, b: &Item| (a.breadth + b.breadth) / 2.0 + if a.node.is_some() && b.node.is_some() { NODE_GAP } else { DUMMY_GAP };
    for layer in &layers {
        let mut x = 0.0;
        for (i, &v) in layer.iter().enumerate() {
            if i > 0 {
                x += gap(&items[layer[i - 1]], &items[v]);
            }
            items[v].pos = x;
        }
        let shift = x / 2.0;
        layer.iter().for_each(|&v| items[v].pos -= shift);
    }
    let neighbors: Vec<Vec<usize>> = (0..items.len()).map(|v| [&g.up[v][..], &g.down[v][..]].concat()).collect();
    for _ in 0..40 {
        for layer in &layers {
            let wanted: Vec<f32> = layer
                .iter()
                .map(|&v| match neighbors[v].len() {
                    0 => items[v].pos,
                    k => neighbors[v].iter().map(|&w| items[w].pos).sum::<f32>() / k as f32,
                })
                .collect();
            let gaps: Vec<f32> = layer.windows(2).map(|w| gap(&items[w[0]], &items[w[1]])).collect();
            for (v, pos) in layer.iter().zip(spread(&wanted, &gaps)) {
                items[*v].pos = pos;
            }
        }
    }

    // Straight runs: items linked one to one (the upper with one link down,
    // the lower with one link up) go to one position where it fits.
    let mut run: Vec<usize> = (0..items.len()).collect();
    fn root(run: &mut [usize], v: usize) -> usize {
        let mut v = v;
        while run[v] != v {
            run[v] = run[run[v]];
            v = run[v];
        }
        v
    }
    for &(a, b) in &links {
        if g.down[a].len() == 1 && g.up[b].len() == 1 {
            let (ra, rb) = (root(&mut run, a), root(&mut run, b));
            run[rb] = ra;
        }
    }
    let mut runs: HashMap<usize, Vec<usize>> = HashMap::new();
    for v in 0..items.len() {
        let r = root(&mut run, v);
        runs.entry(r).or_default().push(v);
    }
    let place: HashMap<usize, (usize, usize)> =
        layers.iter().enumerate().flat_map(|(r, layer)| layer.iter().enumerate().map(move |(i, &v)| (v, (r, i)))).collect();
    let mut runs: Vec<Vec<usize>> = runs.into_values().filter(|run| run.len() > 1).collect();
    runs.sort();
    for _ in 0..4 {
        for members in &runs {
            // The mean first, then each member's own place, nearest first.
            let mean = members.iter().map(|&v| items[v].pos).sum::<f32>() / members.len() as f32;
            let mut targets: Vec<f32> = members.iter().map(|&v| items[v].pos).collect();
            targets.sort_by(|a, b| (a - mean).abs().total_cmp(&(b - mean).abs()));
            targets.insert(0, mean);
            let fits = |target: f32| {
                members.iter().all(|&v| {
                    let (r, i) = place[&v];
                    let layer = &layers[r];
                    let left = i == 0 || target - items[layer[i - 1]].pos >= gap(&items[layer[i - 1]], &items[v]) - 0.01;
                    let right = i + 1 == layer.len() || items[layer[i + 1]].pos - target >= gap(&items[v], &items[layer[i + 1]]) - 0.01;
                    left && right
                })
            };
            if let Some(target) = targets.into_iter().find(|&t| fits(t)) {
                members.iter().for_each(|&v| items[v].pos = target);
            }
        }
    }

    // Where an edge meets a box: on the side facing the other end, spread
    // along it in the order of the other ends, so a fan doesn't leave from
    // one point. Diamonds and circles take their edges at the tip.
    let side = |v: usize| match items[v].node {
        Some(node) if matches!(d.nodes[node].shape, Shape::Rect | Shape::Round) => {
            let (w, h) = boxes[node];
            if horizontal { h } else { w }
        }
        _ => 0.0,
    };
    let mut port: HashMap<(usize, usize), f32> = HashMap::new();
    for v in 0..items.len() {
        for lower in [false, true] {
            let mut others = if lower { g.down[v].clone() } else { g.up[v].clone() };
            others.sort_by(|&a, &b| items[a].pos.total_cmp(&items[b].pos));
            let room = (side(v) - 2.0 * PORT_INSET).max(0.0);
            let step = if others.len() > 1 { (room / (others.len() - 1) as f32).min(PORT_GAP) } else { 0.0 };
            for (i, &other) in others.iter().enumerate() {
                port.insert((v, other), (i as f32 - (others.len() - 1) as f32 / 2.0) * step);
            }
        }
    }
    let at = |v: usize, other: usize| items[v].pos + port[&(v, other)];

    // Across the layers: ranks apart by at least RANK_GAP. Where an edge
    // between them would run flat, as a wide fan's do, they all go across
    // on tracks between the ranks; otherwise the ranks move further apart
    // where an edge would pass over a box on its way.
    let mut line = vec![0.0f32; ranks];
    let mut track: HashMap<(usize, usize), f32> = HashMap::new();
    for r in 1..ranks {
        let (upper, lower) = (&layers[r - 1], &layers[r]);
        let deepest = |layer: &[usize]| layer.iter().map(|&v| items[v].depth).fold(0.0, f32::max);
        let mut gap = (deepest(upper) + deepest(lower)) / 2.0 + RANK_GAP;
        let links: Vec<(usize, usize)> = upper.iter().flat_map(|&a| g.down[a].iter().map(move |&b| (a, b))).collect();
        if links.iter().any(|&(a, b)| (at(b, a) - at(a, b)).abs() > FLAT * gap) {
            let bent: Vec<(usize, usize)> = links.into_iter().filter(|&(a, b)| (at(b, a) - at(a, b)).abs() >= 1.0).collect();
            let spans: Vec<(f32, f32)> = bent.iter().map(|&(a, b)| (at(a, b), at(b, a))).collect();
            let tracks = tracks(&spans);
            let top = line[r - 1] + deepest(upper) / 2.0 + CHANNEL;
            for (&link, &k) in bent.iter().zip(&tracks) {
                track.insert(link, top + k as f32 * TRACK_GAP);
            }
            let count = tracks.iter().max().map_or(0, |k| k + 1);
            gap = gap.max((deepest(upper) + deepest(lower)) / 2.0 + 2.0 * CHANNEL + count.saturating_sub(1) as f32 * TRACK_GAP);
            line[r] = line[r - 1] + gap;
            continue;
        }
        for (a, b) in links {
            let (pa, pb) = (at(a, b), at(b, a));
            let (da, db) = (items[a].depth / 2.0, items[b].depth / 2.0);
            if (pb - pa).abs() < 1.0 {
                continue;
            }
            // How far along the edge, from 0 at `a` to 1 at `b`, it passes
            // the side of box `c` nearer `toward`.
            let reach = |c: usize, toward: f32| {
                let half = items[c].breadth / 2.0 - if items[c].node.is_some() { 0.0 } else { LABEL_GAP };
                (items[c].pos - (items[c].pos - toward).signum() * half - pa) / (pb - pa)
            };
            // The edge leaves `a` at da and reaches `b` at gap - db: above
            // each box of the lower rank before it gets there...
            for &c in lower.iter().filter(|&&c| c != b && items[c].depth > 0.0) {
                let t = reach(c, pb);
                if t > 0.0 && t < 1.0 {
                    gap = gap.max(da + (items[c].depth / 2.0 + CLEAR - t * db) / (1.0 - t));
                }
            }
            // ...and below each box of the upper rank it passes.
            for &c in upper.iter().filter(|&&c| c != a && items[c].depth > 0.0) {
                let t = reach(c, pa);
                if t > 0.0 && t < 1.0 {
                    gap = gap.max((items[c].depth / 2.0 + CLEAR - da) / t + da + db);
                }
            }
        }
        line[r] = line[r - 1] + gap;
    }

    let place = |along: f32, across: f32| match d.direction {
        Direction::Down => (along, across),
        Direction::Up => (along, -across),
        Direction::Right => (across, along),
        Direction::Left => (-across, along),
    };
    let mut centers: Vec<(f32, f32)> = items.iter().map(|i| place(i.pos, line[i.rank])).collect();
    // Each edge from port to port through its points, turning onto its
    // tracks, and split at its label.
    let end = |v: usize, other: usize| {
        let toward = if items[other].rank > items[v].rank { 1.0 } else { -1.0 };
        place(at(v, other), line[items[v].rank] + toward * items[v].depth / 2.0)
    };
    let corners = |u: usize, w: usize| {
        let link = if items[u].rank < items[w].rank { (u, w) } else { (w, u) };
        track.get(&link).map(|&across| [place(at(u, w), across), place(at(w, u), across)])
    };
    for route in &mut routes {
        let chain = &route.items;
        let mut piece = vec![end(chain[0], chain[1])];
        for i in 1..chain.len() {
            piece.extend(corners(chain[i - 1], chain[i]).into_iter().flatten());
            if i == chain.len() - 1 {
                piece.push(end(chain[i], chain[i - 1]));
            } else if route.label == Some(chain[i]) {
                piece.push(end(chain[i], chain[i - 1]));
                route.pieces.push(std::mem::take(&mut piece));
                piece.push(end(chain[i], chain[i + 1]));
            } else {
                piece.push(centers[chain[i]]);
            }
        }
        route.pieces.push(piece);
    }
    let sizes: Vec<(f32, f32)> = items.iter().map(|i| if horizontal { (i.depth, i.breadth) } else { (i.breadth, i.depth) }).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (c, s) in centers.iter().zip(&sizes) {
        x0 = x0.min(c.0 - s.0 / 2.0);
        x1 = x1.max(c.0 + s.0 / 2.0);
        y0 = y0.min(c.1 - s.1 / 2.0);
        y1 = y1.max(c.1 + s.1 / 2.0);
    }
    for c in centers.iter_mut().chain(routes.iter_mut().flat_map(|r| r.pieces.iter_mut().flatten())) {
        c.0 += MARGIN - x0;
        c.1 += MARGIN - y0;
    }
    Layout {
        #[cfg(test)]
        items,
        routes,
        boxes,
        ranks: rank,
        sinks,
        sources,
        centers,
        width: x1 - x0 + 2.0 * MARGIN,
        height: y1 - y0 + 2.0 * MARGIN,
    }
}

/// Tracks for edges across the room between two ranks, each running along
/// the ranks from its start (on the upper rank) to its end, numbered from
/// the top. Edges that overlap take different tracks, and one whose start
/// or end falls within another's span goes above or below it so it doesn't
/// cross the other's ends; where that can't hold for all, the fewest wait.
fn tracks(spans: &[(f32, f32)]) -> Vec<usize> {
    let n = spans.len();
    let within = |x: f32, (a, b): (f32, f32)| x > a.min(b) && x < a.max(b);
    let overlap = |s: usize, t: usize| {
        let ((a, b), (c, d)) = (spans[s], spans[t]);
        a.min(b) < c.max(d) + TRACK_GAP && c.min(d) < a.max(b) + TRACK_GAP
    };
    // above[t]: the edges that go above t, and below[s] the other way.
    let mut above = vec![Vec::new(); n];
    let mut below = vec![Vec::new(); n];
    for s in 0..n {
        for t in (0..n).filter(|&t| t != s) {
            if within(spans[s].0, spans[t]) {
                above[t].push(s);
                below[s].push(t);
            }
            if within(spans[s].1, spans[t]) {
                above[s].push(t);
                below[t].push(s);
            }
        }
    }
    let mut waiting: Vec<usize> = above.iter().map(Vec::len).collect();
    let mut track = vec![usize::MAX; n];
    while let Some(t) = (0..n).filter(|&t| track[t] == usize::MAX).min_by_key(|&t| waiting[t]) {
        let floor = above[t].iter().filter(|&&s| track[s] != usize::MAX).map(|&s| track[s] + 1).max().unwrap_or(0);
        track[t] = (floor..).find(|&k| !(0..n).any(|s| track[s] == k && overlap(s, t))).unwrap();
        for &u in &below[t] {
            waiting[u] = waiting[u].saturating_sub(1);
        }
    }
    track
}

/// Each item's neighbors in the rank above and in the rank below.
struct Graph {
    up: Vec<Vec<usize>>,
    down: Vec<Vec<usize>>,
}

const SWEEPS: usize = 24;

/// Crossings first, then pairs out of their written order: fewer is better.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Score {
    crossings: usize,
    out_of_order: usize,
}

impl Score {
    const WORST: Score = Score { crossings: usize::MAX, out_of_order: usize::MAX };

    fn of(layers: &[Vec<usize>], g: &Graph, index: &[usize], written: &[usize]) -> Score {
        Score {
            crossings: layers.iter().map(|layer| crossings_below(layer, g, index)).sum(),
            out_of_order: layers.iter().map(|layer| inversions(layer.iter().map(|&v| written[v]))).sum(),
        }
    }
}

/// Barycenter sweeps from `layers`, each followed by transposing; returns the
/// best order seen.
fn order(mut layers: Vec<Vec<usize>>, g: &Graph, written: &[usize]) -> (Vec<Vec<usize>>, Score) {
    // Where each item is in its rank.
    let mut index = vec![0; written.len()];
    let reindex = |index: &mut [usize], layer: &[usize]| layer.iter().enumerate().for_each(|(i, &v)| index[v] = i);
    layers.iter().for_each(|layer| reindex(&mut index, layer));
    let mut best = layers.clone();
    let mut fewest = Score::of(&layers, g, &index, written);
    for sweep in 0..SWEEPS {
        let down = sweep % 2 == 0;
        let ranks: Vec<usize> = if down { (1..layers.len()).collect() } else { (0..layers.len().saturating_sub(1)).rev().collect() };
        for r in ranks {
            let near = if down { &g.up } else { &g.down };
            layers[r] = by_barycenter(&layers[r], near, &index);
            reindex(&mut index, &layers[r]);
        }
        transpose(&mut layers, g, &mut index, written);
        let score = Score::of(&layers, g, &index, written);
        if score < fewest {
            best = layers.clone();
            fewest = score;
        }
    }
    (best, fewest)
}

/// `layer` sorted by the mean place of each item's neighbors in `near`; an
/// item without any keeps its place.
fn by_barycenter(layer: &[usize], near: &[Vec<usize>], index: &[usize]) -> Vec<usize> {
    let mut keyed: Vec<(f32, usize)> = layer
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let key = match near[v].len() {
                0 => i as f32,
                k => near[v].iter().map(|&w| index[w] as f32).sum::<f32>() / k as f32,
            };
            (key, v)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    keyed.into_iter().map(|(_, v)| v).collect()
}

/// Crossings between `layer` and the rank below it.
fn crossings_below(layer: &[usize], g: &Graph, index: &[usize]) -> usize {
    let mut ends: Vec<(usize, usize)> = layer.iter().flat_map(|&a| g.down[a].iter().map(move |&b| (index[a], index[b]))).collect();
    ends.sort_unstable();
    inversions(ends.into_iter().map(|(_, lower)| lower))
}

/// Pairs that come in decreasing order, in O(n log n): a Fenwick tree of
/// the values seen.
fn inversions(values: impl Iterator<Item = usize>) -> usize {
    // Each value by its place among the distinct ones, for a small tree.
    let mut values: Vec<usize> = values.collect();
    let mut distinct = values.clone();
    distinct.sort_unstable();
    distinct.dedup();
    values.iter_mut().for_each(|v| *v = distinct.binary_search(v).unwrap());
    let mut tree = vec![0usize; distinct.len() + 1];
    let mut count = 0;
    for (seen, &v) in values.iter().enumerate() {
        // Those seen so far that are at most v...
        let (mut at, mut not_greater) = (v + 1, 0);
        while at > 0 {
            not_greater += tree[at];
            at &= at - 1;
        }
        // ...and the rest are greater.
        count += seen - not_greater;
        let mut at = v + 1;
        while at < tree.len() {
            tree[at] += 1;
            at += at.isolate_lowest_one();
        }
    }
    count
}

#[cfg(test)]
fn crossings(layers: &[Vec<usize>], links: &[(usize, usize)]) -> usize {
    let items = layers.iter().flatten().max().map_or(0, |m| m + 1);
    let mut g = Graph { up: vec![Vec::new(); items], down: vec![Vec::new(); items] };
    for &(a, b) in links {
        g.down[a].push(b);
        g.up[b].push(a);
    }
    let mut index = vec![0; items];
    layers.iter().for_each(|layer| layer.iter().enumerate().for_each(|(i, &v)| index[v] = i));
    layers.iter().map(|layer| crossings_below(layer, &g, &index)).sum()
}

/// Swaps neighbors in a rank while that removes crossings, or puts them in
/// their written order with no more crossings. Only the edges of the two
/// swapped items change their crossings.
fn transpose(layers: &mut [Vec<usize>], g: &Graph, index: &mut [usize], written: &[usize]) {
    // Crossings among the edges of u and v with u left of v.
    let between = |index: &[usize], u: usize, v: usize| {
        [(&g.up[u], &g.up[v]), (&g.down[u], &g.down[v])]
            .iter()
            .map(|(left, right)| left.iter().map(|&p| right.iter().filter(|&&q| index[p] > index[q]).count()).sum::<usize>())
            .sum::<usize>()
    };
    // A rank is gone over again only when it or a rank next to it changed.
    let mut dirty = vec![true; layers.len()];
    while let Some(r) = dirty.iter().position(|&d| d) {
        dirty[r] = false;
        let layer = &mut layers[r];
        for i in 1..layer.len() {
            let (u, v) = (layer[i - 1], layer[i]);
            let (kept, swapped) = (between(index, u, v), between(index, v, u));
            if swapped < kept || (swapped == kept && written[v] < written[u]) {
                layer.swap(i - 1, i);
                (index[u], index[v]) = (i, i - 1);
                dirty[r] = true;
                dirty[r.saturating_sub(1)] |= r > 0;
                if r + 1 < dirty.len() {
                    dirty[r + 1] = true;
                }
            }
        }
    }
}

/// The positions nearest to `wanted`, in least squares, that keep each one
/// at least its gap after the previous: pool adjacent violators on the
/// positions less the gaps before them.
fn spread(wanted: &[f32], gaps: &[f32]) -> Vec<f32> {
    let mut offset = vec![0.0f32; wanted.len()];
    for i in 1..wanted.len() {
        offset[i] = offset[i - 1] + gaps[i - 1];
    }
    // Blocks of (mean, count).
    let mut blocks: Vec<(f32, usize)> = Vec::new();
    for (w, o) in wanted.iter().zip(&offset) {
        blocks.push((w - o, 1));
        while blocks.len() > 1 && blocks[blocks.len() - 2].0 > blocks[blocks.len() - 1].0 {
            let (m2, c2) = blocks.pop().unwrap();
            let (m1, c1) = blocks.pop().unwrap();
            blocks.push(((m1 * c1 as f32 + m2 * c2 as f32) / (c1 + c2) as f32, c1 + c2));
        }
    }
    blocks.iter().flat_map(|&(m, c)| std::iter::repeat_n(m, c)).zip(&offset).map(|(m, o)| m + o).collect()
}

// Drawing.

#[derive(Clone)]
struct Svg {
    text: String,
    width: f32,
}

const STROKE: &str = "#7c8597";
const FILL: &str = "#eef1f8";
const INK: &str = "#1f2329";
const LABEL_BORDER: &str = "#d3d8e0";

fn render(source: &str) -> Result<Svg, String> {
    let mut d = parse(source)?;
    if d.nodes.is_empty() {
        return Err("no states or nodes".into());
    }
    let mut l = layout(&d);
    // Left to right wider than a preview goes top to bottom instead, when
    // that is narrower: drawn smaller it would not be read.
    if l.width > COLUMN && matches!(d.direction, Direction::Right | Direction::Left) {
        let along = d.direction;
        d.direction = if along == Direction::Right { Direction::Down } else { Direction::Up };
        let turned = layout(&d);
        if turned.width < l.width {
            l = turned;
        } else {
            d.direction = along;
        }
    }
    // A huge diagram is drawn smaller: gpui draws an SVG at twice its size,
    // and a side past the GPU's largest texture isn't drawn at all.
    let scale = (MAX_SIDE / l.width.max(l.height)).min((MAX_AREA / (l.width * l.height)).sqrt()).min(1.0);
    let mut s = String::new();
    let _ = write!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" viewBox=\"0 0 {w:.1} {h:.1}\" \
         font-family=\"Helvetica, Arial, sans-serif\" font-size=\"{FONT}\">\
         <defs><marker id=\"a\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" markerHeight=\"7\" orient=\"auto\">\
         <path d=\"M0,0 L10,5 L0,10 z\" fill=\"{STROKE}\"/></marker></defs>",
        l.width * scale,
        l.height * scale,
        w = l.width,
        h = l.height,
    );
    let size_of = |v: usize| l.boxes[v];
    let horizontal = matches!(d.direction, Direction::Right | Direction::Left);
    for route in &l.routes {
        let edge = &d.edges[route.edge];
        for (i, piece) in route.pieces.iter().enumerate() {
            path(&mut s, piece, horizontal, edge, edge.arrow && i + 1 == route.pieces.len());
        }
    }
    for (e, edge) in d.edges.iter().enumerate().filter(|(_, e)| e.from == e.to) {
        let (cx, cy) = l.centers[edge.from];
        let (w, h) = size_of(edge.from);
        let loops = d.edges[..e].iter().filter(|o| o.from == edge.from && o.to == edge.from).count() as f32;
        let reach = LOOP * 0.8 + loops * 10.0;
        // One cubic out and back, reaching `reach` from the box.
        let out = reach * 4.0 / 3.0;
        let [a, b, c, e] = if horizontal {
            let y = cy + h / 2.0;
            [(cx - w / 4.0, y), (cx - w / 4.0, y + out), (cx + w / 4.0, y + out), (cx + w / 4.0, y)]
        } else {
            let x = cx + w / 2.0;
            [(x, cy - h / 4.0), (x + out, cy - h / 4.0), (x + out, cy + h / 4.0), (x, cy + h / 4.0)]
        };
        let _ = write!(s, "<path d=\"M{:.1},{:.1} C{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}", a.0, a.1, b.0, b.1, c.0, c.1, e.0, e.1);
        stroke(&mut s, edge, edge.arrow);
        if !edge.label.is_empty() {
            let (tw, th) = text_size(&edge.label);
            let at = if horizontal { (cx, cy + h / 2.0 + reach + th / 2.0 + 4.0) } else { (cx + w / 2.0 + reach + tw / 2.0 + 8.0, cy) };
            label(&mut s, at, &edge.label);
        }
    }
    for route in &l.routes {
        if let Some(item) = route.label {
            label(&mut s, l.centers[item], &d.edges[route.edge].label);
        }
    }
    for (v, node) in d.nodes.iter().enumerate() {
        let (cx, cy) = l.centers[v];
        let (w, h) = size_of(v);
        match node.shape {
            Shape::Start => {
                let _ = write!(s, "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"8\" fill=\"{STROKE}\"/>");
            }
            Shape::End => {
                let _ = write!(
                    s,
                    "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"9.5\" fill=\"none\" stroke=\"{STROKE}\" stroke-width=\"1.5\"/>\
                     <circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"6\" fill=\"{STROKE}\"/>"
                );
            }
            Shape::Diamond => {
                // Corners rounded as the boxes' are.
                let corners = [(cx, cy - h / 2.0), (cx + w / 2.0, cy), (cx, cy + h / 2.0), (cx - w / 2.0, cy)];
                let toward = |from: (f32, f32), to: (f32, f32)| {
                    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                    let t = 5.0 / (dx * dx + dy * dy).sqrt();
                    (from.0 + dx * t, from.1 + dy * t)
                };
                s.push_str("<path d=\"");
                for i in 0..4 {
                    let (prev, at, next) = (corners[(i + 3) % 4], corners[i], corners[(i + 1) % 4]);
                    let (a, b) = (toward(at, prev), toward(at, next));
                    let _ = write!(s, "{}{:.1},{:.1} Q{:.1},{:.1} {:.1},{:.1} ", if i == 0 { "M" } else { "L" }, a.0, a.1, at.0, at.1, b.0, b.1);
                }
                let _ = write!(s, "Z\" fill=\"{FILL}\" stroke=\"{STROKE}\" stroke-width=\"1.2\"/>");
            }
            Shape::Rect | Shape::Round => {
                let radius = if node.shape == Shape::Rect { 3.0 } else { 10.0 };
                let _ = write!(
                    s,
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"{radius}\" fill=\"{FILL}\" stroke=\"{STROKE}\" stroke-width=\"1.2\"/>",
                    cx - w / 2.0,
                    cy - h / 2.0,
                );
            }
        }
        if !matches!(node.shape, Shape::Start | Shape::End) {
            text(&mut s, (cx, cy), &node.label);
        }
    }
    s.push_str("</svg>");
    Ok(Svg { text: s, width: l.width * scale })
}

/// A smooth line through the points, which go one per rank: across the
/// ranks it only moves on, and along them it never swings past the points
/// on either side (a monotone cubic, Fritsch-Carlson). It keeps the room left
/// beside each box, and makes no waves.
fn path(s: &mut String, points: &[(f32, f32)], horizontal: bool, edge: &Edge, arrow: bool) {
    // (along, across) and back.
    let flip = |(x, y): (f32, f32)| if horizontal { (y, x) } else { (x, y) };
    let p: Vec<(f32, f32)> = points.iter().map(|&q| flip(q)).collect();
    if p.windows(2).any(|w| (w[1].1 - w[0].1).abs() < 0.01) {
        return square(s, points, edge, arrow);
    }
    let steps: Vec<f32> = p.windows(2).map(|w| w[1].1 - w[0].1).collect();
    let slopes: Vec<f32> = p.windows(2).zip(&steps).map(|(w, &h)| if h == 0.0 { 0.0 } else { (w[1].0 - w[0].0) / h }).collect();
    let n = p.len();
    let tangent = |k: usize| match k {
        0 => slopes[0],
        k if k == n - 1 => slopes[n - 2],
        k => {
            let (d0, d1) = (slopes[k - 1], slopes[k]);
            if d0 * d1 <= 0.0 {
                return 0.0;
            }
            let (h0, h1) = (steps[k - 1].abs(), steps[k].abs());
            let (w0, w1) = (2.0 * h1 + h0, h1 + 2.0 * h0);
            (w0 + w1) / (w0 / d0 + w1 / d1)
        }
    };
    let _ = write!(s, "<path d=\"M{:.1},{:.1}", points[0].0, points[0].1);
    for k in 0..n - 1 {
        let h = steps[k] / 3.0;
        let c1 = flip((p[k].0 + tangent(k) * h, p[k].1 + h));
        let c2 = flip((p[k + 1].0 - tangent(k + 1) * h, p[k + 1].1 - h));
        let _ = write!(s, " C{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}", c1.0, c1.1, c2.0, c2.1, points[k + 1].0, points[k + 1].1);
    }
    stroke(s, edge, arrow);
}

/// Straight from point to point, the corners rounded: an edge that goes
/// across on a track.
fn square(s: &mut String, points: &[(f32, f32)], edge: &Edge, arrow: bool) {
    let mut p = points.to_vec();
    p.dedup_by(|a, b| (a.0 - b.0).hypot(a.1 - b.1) < 0.01);
    let toward = |from: (f32, f32), to: (f32, f32), by: f32| {
        let t = by / (to.0 - from.0).hypot(to.1 - from.1);
        (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
    };
    let _ = write!(s, "<path d=\"M{:.1},{:.1}", p[0].0, p[0].1);
    for k in 1..p.len().saturating_sub(1) {
        let (a, b, c) = (p[k - 1], p[k], p[k + 1]);
        let r = RADIUS.min((b.0 - a.0).hypot(b.1 - a.1) / 2.0).min((c.0 - b.0).hypot(c.1 - b.1) / 2.0);
        let (into, out) = (toward(b, a, r), toward(b, c, r));
        let _ = write!(s, " L{:.1},{:.1} Q{:.1},{:.1} {:.1},{:.1}", into.0, into.1, b.0, b.1, out.0, out.1);
    }
    let last = p[p.len() - 1];
    let _ = write!(s, " L{:.1},{:.1}", last.0, last.1);
    stroke(s, edge, arrow);
}

/// The end of an edge's `<path`, after its points.
fn stroke(s: &mut String, edge: &Edge, arrow: bool) {
    let _ = write!(
        s,
        "\" fill=\"none\" stroke=\"{STROKE}\" stroke-width=\"1.5\"{}{}/>",
        if edge.dashed { " stroke-dasharray=\"5 4\"" } else { "" },
        if arrow { " marker-end=\"url(#a)\"" } else { "" },
    );
}

fn label_box(lines: &[String]) -> (f32, f32) {
    let (w, h) = text_size(lines);
    (w + 16.0, h + 10.0)
}

/// A label in a white box with a border, readable on any background.
fn label(s: &mut String, (cx, cy): (f32, f32), lines: &[String]) {
    let (w, h) = label_box(lines);
    let _ = write!(
        s,
        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"4\" fill=\"#ffffff\" stroke=\"{LABEL_BORDER}\"/>",
        cx - w / 2.0,
        cy - h / 2.0,
    );
    let mut y = cy - text_size(lines).1 / 2.0;
    for line in lines {
        if line.is_empty() {
            let _ = write!(
                s,
                "<path d=\"M{:.1},{:.1} H{:.1}\" stroke=\"{LABEL_BORDER}\"/>",
                cx - w / 2.0,
                y + RULE / 2.0,
                cx + w / 2.0,
            );
        }
        y += line_height(line);
    }
    text(s, (cx, cy), lines);
}

fn text(s: &mut String, (cx, cy): (f32, f32), lines: &[String]) {
    let mut y = cy - text_size(lines).1 / 2.0;
    for line in lines {
        if !line.is_empty() {
            let _ = write!(
                s,
                "<text x=\"{cx:.1}\" y=\"{:.1}\" text-anchor=\"middle\" fill=\"{INK}\">{}</text>",
                y + LINE / 2.0 + FONT * 0.35,
                escape(line),
            );
        }
        y += line_height(line);
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod limits;

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: &str = "stateDiagram-v2
    [*] --> Trial: signs up
    Trial --> Active: takes a plan and pays
    Trial --> ReadOnly: trial ends with no plan
    Active --> Unpaid: charge refused
    Unpaid --> Active: pays within 7 days
    Unpaid --> ReadOnly: 7 days pass
    ReadOnly --> Active: pays
    ReadOnly --> [*]: 60 or 90 days
";

    const FLOW: &str = "flowchart TD
    A[Takes a plan] --> B{Card kept?}
    B -->|yes| C[Charge]
    B -- no --> D[Payment page]
    C --> E{Paid?}
    E -->|yes| F[Plan active<br>invoice issued]
    E -.->|no| G[The bank's message]
    D --> F
";

    #[test]
    fn states_parse_with_one_start_and_one_end() {
        let d = parse(STATES).unwrap();
        let labels: Vec<&str> = d.nodes.iter().map(|n| n.label[0].as_str()).collect();
        assert_eq!(labels, ["[*] start", "Trial", "Active", "ReadOnly", "Unpaid", "[*] end"]);
        assert_eq!(d.nodes[0].shape, Shape::Start);
        assert_eq!(d.nodes[5].shape, Shape::End);
        assert_eq!(d.edges.len(), 8);
        assert_eq!(d.edges[1].label, ["takes a plan and pays"]);
    }

    #[test]
    fn flowchart_parses_shapes_labels_and_links() {
        let d = parse(FLOW).unwrap();
        assert_eq!(d.nodes[1].shape, Shape::Diamond);
        assert_eq!(d.nodes[1].label, ["Card kept?"]);
        assert_eq!(d.nodes[5].label, ["Plan active", "invoice issued"]);
        let link = |from: usize, to: usize| d.edges.iter().find(|e| e.from == from && e.to == to).unwrap();
        assert_eq!(link(1, 2).label, ["yes"]);
        assert_eq!(link(1, 3).label, ["no"]);
        assert!(link(4, 6).dashed && link(4, 6).arrow);
        assert!(link(3, 5).label.is_empty());
    }

    #[test]
    fn a_chain_is_one_edge_per_link() {
        let d = parse("graph LR\n  A --> B --- C;").unwrap();
        assert_eq!(d.direction, Direction::Right);
        assert_eq!(d.edges.len(), 2);
        assert!(d.edges[0].arrow && !d.edges[1].arrow);
    }

    #[test]
    fn an_ampersand_links_each_node() {
        let d = parse("flowchart TD\n A --> B & C\n B & C --> D").unwrap();
        let pairs: Vec<(usize, usize)> = d.edges.iter().map(|e| (e.from, e.to)).collect();
        assert_eq!(pairs, [(0, 1), (0, 2), (1, 3), (2, 3)]);
    }

    /// Left to right that does not fit a preview is drawn top to bottom;
    /// one that fits stays as written.
    #[test]
    fn too_wide_left_to_right_turns_down() {
        let wide = "flowchart LR\n A[a long first box] --> B[a long second box] --> C[a long third box] --> D[a long fourth box] --> E[a long fifth box]";
        let svg = render(wide).unwrap();
        assert!(svg.width <= COLUMN, "{}", svg.width);
        let narrow = "flowchart LR\n A --> B";
        let svg = render(narrow).unwrap();
        assert!(svg.width > render("flowchart TD\n A --> B").unwrap().width);
    }

    #[test]
    fn what_it_cannot_draw_is_an_error_naming_the_line() {
        for (source, part) in [
            ("sequenceDiagram\n A->>B: hi", "sequenceDiagram"),
            ("flowchart TD\n subgraph one", "subgraph one"),
            ("stateDiagram-v2\n state Big {", "state Big {"),
            ("flowchart TD\n A --> ", "expected a node"),
            ("flowchart TD\n A[open --> B", "`[` with no `]`"),
            ("flowchart ZZ", "unknown direction"),
            ("", "empty"),
        ] {
            let error = parse(source).unwrap_err();
            assert!(error.contains(part), "{source:?}: {error}");
        }
    }

    /// No two boxes of a rank overlap, and every edge goes down the ranks
    /// except those turned to break a cycle.
    #[test]
    fn the_layout_keeps_boxes_apart() {
        for source in [STATES, FLOW] {
            let d = parse(source).unwrap();
            let l = layout(&d);
            for (i, a) in l.items.iter().enumerate() {
                for b in &l.items[i + 1..] {
                    if a.rank == b.rank && (a.breadth > 0.0 && b.breadth > 0.0) {
                        assert!((a.pos - b.pos).abs() >= (a.breadth + b.breadth) / 2.0 - 0.01, "{source}");
                    }
                }
            }
            assert!(l.routes.iter().all(|r| r.items.windows(2).all(|w| l.items[w[0]].rank.abs_diff(l.items[w[1]].rank) == 1)));
            assert_eq!(l.routes.len(), d.edges.len());
        }
    }

    #[test]
    fn a_tree_has_no_crossings() {
        let d = parse("flowchart TD\n A --> B\n A --> C\n B --> D\n C --> E\n B --> F\n C --> G").unwrap();
        let l = layout(&d);
        let ranks = l.items.iter().map(|i| i.rank).max().unwrap() + 1;
        let mut layers = vec![Vec::new(); ranks];
        let mut sorted: Vec<usize> = (0..l.items.len()).collect();
        sorted.sort_by(|&a, &b| l.items[a].pos.total_cmp(&l.items[b].pos));
        for v in sorted {
            layers[l.items[v].rank].push(v);
        }
        let links: Vec<(usize, usize)> = l.routes.iter().flat_map(|r| r.items.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>()).collect();
        assert_eq!(crossings(&layers, &links), 0);
    }

    /// With no crossing at stake, the branches go left to right as written.
    #[test]
    fn branches_keep_their_written_order() {
        for (source, left, right) in [
            ("flowchart TD\n Q{Ok?} -->|yes| Y\n Q -->|no| N", 1, 2),
            ("flowchart TD\n Q{Ok?} -->|no| N\n Q -->|yes| Y", 1, 2),
            ("flowchart TD\n A --> B\n A --> C\n A --> D", 1, 3),
        ] {
            let l = layout(&parse(source).unwrap());
            assert!(l.centers[left].0 < l.centers[right].0, "{source}");
        }
        let d = parse(FLOW).unwrap();
        let l = layout(&d);
        let x = |label: &str| l.centers[d.nodes.iter().position(|n| n.label[0] == label).unwrap()].0;
        assert!(x("Charge") < x("Payment page"), "yes is written before no");
    }

    #[test]
    fn inversions_count_the_pairs_out_of_order() {
        assert_eq!(inversions([3, 1, 2].into_iter()), 2);
        assert_eq!(inversions([1, 1, 2, 2].into_iter()), 0);
        assert_eq!(inversions([5, 4, 3, 2, 1].into_iter()), 10);
        assert_eq!(inversions([1_000_000, 0].into_iter()), 1);
        assert_eq!(inversions(std::iter::empty()), 0);
    }

    /// A fan's edges nest: the one going furthest runs on top, so none
    /// crosses another's ends, and the two sides share the tracks.
    #[test]
    fn a_fan_nests_on_its_tracks() {
        let spans = [(-12.0, -300.0), (0.0, -200.0), (12.0, -100.0), (24.0, 100.0), (36.0, 200.0)];
        assert_eq!(tracks(&spans), [0, 1, 2, 1, 0]);
    }

    #[test]
    fn spread_keeps_order_and_gaps() {
        assert_eq!(spread(&[0.0, 0.0, 0.0], &[10.0, 10.0]), [-10.0, 0.0, 10.0]);
        assert_eq!(spread(&[0.0, 50.0], &[10.0]), [0.0, 50.0]);
    }

    #[test]
    fn a_block_becomes_an_image_and_the_rest_stays() {
        let markdown = format!("# Accounts\n\nText.\n```mermaid\n{STATES}```\nAfter.\n");
        let out = with_diagrams(&markdown, FONT);
        assert!(out.starts_with("# Accounts\n\nText.\n\n<p align=\"center\"><img src=\"data:image/svg+xml,%3Csvg"));
        assert!(out.ends_with("alt=\"diagram\"></p>\n\nAfter.\n"));
        assert!(!out.contains("```"));
    }

    #[test]
    fn a_block_it_cannot_draw_stays_as_code_under_the_reason() {
        let markdown = "~~~~mermaid\npie\n~~~~\n";
        assert_eq!(
            with_diagrams(markdown, FONT),
            "\n> Diagram not drawn: `pie`: only stateDiagram and flowchart are drawn\n\n~~~~mermaid\npie\n~~~~\n"
        );
        assert!(matches!(with_diagrams("```rust\nfn mermaid() {}\n```\n", FONT), Cow::Borrowed(_)));
    }

    #[test]
    fn arrows_between_the_same_boxes_are_one_with_their_labels_stacked() {
        let d = parse("stateDiagram-v2\n A --> B: pays\n A --> B: confirmed\n A --> B\n B --> A: moved").unwrap();
        assert_eq!(d.edges.len(), 2);
        assert_eq!(d.edges[0].label, ["pays", "", "confirmed"]);
        assert_eq!(d.edges[1].label, ["moved"]);
    }

    #[test]
    fn a_long_label_breaks_into_even_lines() {
        let lines = wrap(&["paid after it lapsed, hour still free, same booking".to_string()]);
        assert_eq!(lines, ["paid after it lapsed, hour", "still free, same booking"]);
        assert!(lines.iter().all(|l| text_width(l) <= WRAP));
        assert_eq!(wrap(&["paid".to_string()]), ["paid"]);
    }

    #[test]
    fn labels_are_escaped() {
        let svg = render("flowchart TD\n A[a < b & \"c\"] --> B").unwrap();
        assert!(svg.text.contains(">a &lt; b &amp; &quot;c&quot;</text>"));
    }

    /// The preview asks for the diagram's image and gets an SVG that draws.
    /// A diagram wider than the column keeps its size, its text as big as
    /// the preview's, and scrolls sideways.
    #[gpui_kit::test]
    fn a_wide_diagram_is_not_shrunk(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::text::{TextView, TextViewState};
        use gpui_kit::*;

        struct Narrow {
            text: Entity<TextViewState>,
        }
        impl Render for Narrow {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().w(px(300.)).child(TextView::new(&self.text))
            }
        }

        cx.update(gpui_kit::init);
        let source = "flowchart TD\n A[A wide box] --> B[Another wide box]\n A --> C[A third wide box]\n A --> D[And a fourth one]\n";
        let width = render(source).unwrap().width;
        assert!(width > 400.0, "{width}");
        let markdown = with_diagrams(&format!("```mermaid\n{source}```\n"), FONT).into_owned();
        let (_, cx) = cx.add_window_view(|_, cx| Narrow { text: cx.new(|cx| TextViewState::markdown(&markdown, cx)) });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("image diagram").expect("the diagram is drawn").size.width, px(width.ceil()));
    }

    #[gpui_kit::test]
    fn the_preview_draws_the_image(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::text::{TextView, TextViewState};
        use gpui_kit::*;
        use std::sync::{Arc, Mutex};

        struct Preview {
            text: Entity<TextViewState>,
            asked: Arc<Mutex<Vec<String>>>,
        }
        impl Render for Preview {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let asked = self.asked.clone();
                TextView::new(&self.text)
                    .resolve_image_source(move |url| {
                        asked.lock().unwrap().push(url.to_string());
                        None
                    })
                    .size_full()
            }
        }

        cx.update(gpui_kit::init);
        let markdown = with_diagrams(&format!("Before.\n```mermaid\n{FLOW}```\n"), FONT).into_owned();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (_, cx) = cx.add_window_view(|_, cx| Preview { text: cx.new(|cx| TextViewState::markdown(&markdown, cx)), asked: asked.clone() });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let url = asked.lock().unwrap().first().cloned().expect("the preview asks for the diagram");
        let image = super::super::markdown_images::data_url_image(&url).expect("a data URL it decodes");
        assert_eq!(image.format(), ImageFormat::Svg);
        let rendered = cx.update(|_, cx| image.to_image_data(cx.svg_renderer())).expect("the SVG draws");
        assert!(rendered.size(0).width.0 > 200);
    }
}
