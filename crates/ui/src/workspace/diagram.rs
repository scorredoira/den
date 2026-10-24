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
        match render(&block) {
            Ok(svg) => {
                let _ = write!(
                    out,
                    "\n<p align=\"center\"><img src=\"data:image/svg+xml,{}\" width=\"{}\" alt=\"diagram\"></p>\n\n",
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
        let mut from = flow_node(&mut p, &mut b, line)?;
        loop {
            p.skip_spaces();
            if p.done() {
                break;
            }
            let (dashed, arrow, label) = flow_link(&mut p, line)?;
            p.skip_spaces();
            let to = flow_node(&mut p, &mut b, line)?;
            b.edges.push(Edge { from, to, label, dashed, arrow });
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
}

struct Layout {
    /// For the tests: where each box went.
    #[cfg(test)]
    items: Vec<Item>,
    routes: Vec<Route>,
    /// Item centers in the SVG, and the drawing's size.
    centers: Vec<(f32, f32)>,
    width: f32,
    height: f32,
}

fn layout(d: &Diagram) -> Layout {
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
    let mut rank = vec![0usize; n];
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
    for v in 0..n {
        let ins = flow.iter().any(|&e| ends(e).1 == v);
        let outs: Vec<usize> = flow.iter().filter(|&&e| ends(e).0 == v).map(|&e| rank[ends(e).1]).collect();
        if !ins && !outs.is_empty() {
            rank[v] = outs.iter().min().unwrap() - 2;
        }
    }
    for (v, item) in items.iter_mut().enumerate() {
        item.rank = rank[v];
    }

    // Each edge goes through a point per rank it crosses; its label is the
    // middle one on a label rank.
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
        let middle = if items[chain[middle]].rank % 2 == 1 { middle } else { middle - 1 };
        let label = (!d.edges[e].label.is_empty()).then(|| {
            let item = &mut items[chain[middle]];
            let (w, h) = label_box(&d.edges[e].label);
            (item.breadth, item.depth) = if horizontal { (h + LABEL_GAP * 2.0, w) } else { (w + LABEL_GAP * 2.0, h) };
            chain[middle]
        });
        if reversed[e] {
            chain.reverse();
        }
        routes.push(Route { edge: e, items: chain, label });
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
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); ranks];
    let mut seen = vec![false; items.len()];
    let mut queue: Vec<usize> = (0..n).filter(|&v| !links.iter().any(|l| l.1 == v)).collect();
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
        queue.extend(links.iter().filter(|l| l.0 == v).map(|l| l.1));
    }
    // Links between rank r and r + 1, upper end first.
    let mut below: Vec<Vec<(usize, usize)>> = vec![Vec::new(); ranks];
    for &(a, b) in &links {
        below[items[a].rank].push((a, b));
    }
    let mut starts = vec![layers.clone(), layers.iter().map(|l| l.iter().rev().copied().collect()).collect()];
    let mut seed = 0x9e37_79b9u32;
    for _ in 0..6 {
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
    let mut fewest = (usize::MAX, usize::MAX);
    for start in starts {
        let (ordered, score) = order(start, &below, &written);
        if score < fewest {
            (best, fewest) = (ordered, score);
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
    let mut neighbors: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for &(a, b) in &links {
        neighbors[a].push(b);
        neighbors[b].push(a);
    }
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
        let downs = links.iter().filter(|l| l.0 == a).count();
        let ups = links.iter().filter(|l| l.1 == b).count();
        if downs == 1 && ups == 1 {
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

    // Across the layers: each rank as deep as its deepest item.
    let mut depth = vec![0.0f32; ranks];
    for item in &items {
        depth[item.rank] = depth[item.rank].max(item.depth);
    }
    let mut line = vec![0.0f32; ranks];
    for r in 1..ranks {
        line[r] = line[r - 1] + (depth[r - 1] + depth[r]) / 2.0 + RANK_GAP;
    }

    let place = |along: f32, across: f32| match d.direction {
        Direction::Down => (along, across),
        Direction::Up => (along, -across),
        Direction::Right => (across, along),
        Direction::Left => (-across, along),
    };
    let mut centers: Vec<(f32, f32)> = items.iter().map(|i| place(i.pos, line[i.rank])).collect();
    let sizes: Vec<(f32, f32)> = items.iter().map(|i| if horizontal { (i.depth, i.breadth) } else { (i.breadth, i.depth) }).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (c, s) in centers.iter().zip(&sizes) {
        x0 = x0.min(c.0 - s.0 / 2.0);
        x1 = x1.max(c.0 + s.0 / 2.0);
        y0 = y0.min(c.1 - s.1 / 2.0);
        y1 = y1.max(c.1 + s.1 / 2.0);
    }
    for c in &mut centers {
        c.0 += MARGIN - x0;
        c.1 += MARGIN - y0;
    }
    Layout {
        #[cfg(test)]
        items,
        routes,
        centers,
        width: x1 - x0 + 2.0 * MARGIN,
        height: y1 - y0 + 2.0 * MARGIN,
    }
}

/// Barycenter sweeps from `layers`, each followed by transposing, and the
/// order seen with the fewest crossings, then the fewest pairs out of their
/// written order.
fn order(mut layers: Vec<Vec<usize>>, below: &[Vec<(usize, usize)>], written: &[usize]) -> (Vec<Vec<usize>>, (usize, usize)) {
    let ranks = layers.len();
    let total = |layers: &[Vec<usize>]| {
        let crossings = (0..ranks.saturating_sub(1)).map(|r| pair_crossings(&layers[r], &layers[r + 1], &below[r])).sum::<usize>();
        let swapped = layers
            .iter()
            .map(|layer| (0..layer.len()).map(|i| layer[i + 1..].iter().filter(|&&b| written[b] < written[layer[i]]).count()).sum::<usize>())
            .sum::<usize>();
        (crossings, swapped)
    };
    let mut best = layers.clone();
    let mut fewest = total(&layers);
    for sweep in 0..24 {
        let down = sweep % 2 == 0;
        let ranks_in_order: Vec<usize> = if down { (1..ranks).collect() } else { (0..ranks.saturating_sub(1)).rev().collect() };
        for r in ranks_in_order {
            let (near, links) = if down { (r - 1, &below[r - 1]) } else { (r + 1, &below[r]) };
            let mut keyed: Vec<(f32, usize)> = layers[r]
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    let across: Vec<f32> = links
                        .iter()
                        .filter_map(|&(a, b)| match (a == v, b == v) {
                            (true, _) => Some(b),
                            (_, true) => Some(a),
                            _ => None,
                        })
                        .filter_map(|w| layers[near].iter().position(|&x| x == w))
                        .map(|p| p as f32)
                        .collect();
                    let key = if across.is_empty() { i as f32 } else { across.iter().sum::<f32>() / across.len() as f32 };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            layers[r] = keyed.into_iter().map(|(_, v)| v).collect();
        }
        transpose(&mut layers, below, written);
        let score = total(&layers);
        if score < fewest {
            (best, fewest) = (layers.clone(), score);
        }
    }
    (best, fewest)
}

/// Crossings between two adjacent ranks.
fn pair_crossings(upper: &[usize], lower: &[usize], links: &[(usize, usize)]) -> usize {
    let at = |layer: &[usize], v: usize| layer.iter().position(|&x| x == v).unwrap_or(0);
    let ends: Vec<(usize, usize)> = links.iter().map(|&(a, b)| (at(upper, a), at(lower, b))).collect();
    let mut count = 0;
    for (i, a) in ends.iter().enumerate() {
        count += ends[i + 1..].iter().filter(|b| (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1)).count();
    }
    count
}

#[cfg(test)]
fn crossings(layers: &[Vec<usize>], links: &[(usize, usize)]) -> usize {
    layers
        .windows(2)
        .map(|pair| {
            let between: Vec<(usize, usize)> = links.iter().filter(|(a, b)| pair[0].contains(a) && pair[1].contains(b)).copied().collect();
            pair_crossings(&pair[0], &pair[1], &between)
        })
        .sum()
}

/// Swaps neighbors in a rank while that removes crossings, or puts them in
/// their written order with no more crossings; a swap in rank r changes only
/// the crossings with r - 1 and r + 1.
fn transpose(layers: &mut [Vec<usize>], below: &[Vec<(usize, usize)>], written: &[usize]) {
    let around = |layers: &[Vec<usize>], r: usize| {
        let above = if r > 0 { pair_crossings(&layers[r - 1], &layers[r], &below[r - 1]) } else { 0 };
        let under = if r + 1 < layers.len() { pair_crossings(&layers[r], &layers[r + 1], &below[r]) } else { 0 };
        above + under
    };
    let mut improved = true;
    while improved {
        improved = false;
        for r in 0..layers.len() {
            for i in 1..layers[r].len() {
                let before = around(layers, r);
                layers[r].swap(i - 1, i);
                let after = around(layers, r);
                if after < before || (after == before && written[layers[r][i - 1]] < written[layers[r][i]]) {
                    improved = true;
                } else {
                    layers[r].swap(i - 1, i);
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

struct Svg {
    text: String,
    width: f32,
}

const STROKE: &str = "#7c8597";
const FILL: &str = "#eef1f8";
const INK: &str = "#1f2329";
const LABEL_BORDER: &str = "#d3d8e0";

fn render(source: &str) -> Result<Svg, String> {
    let d = parse(source)?;
    if d.nodes.is_empty() {
        return Err("no states or nodes".into());
    }
    let l = layout(&d);
    let mut s = String::new();
    let _ = write!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.1} {h:.1}\" \
         font-family=\"Helvetica, Arial, sans-serif\" font-size=\"{FONT}\">\
         <defs><marker id=\"a\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" markerHeight=\"7\" orient=\"auto\">\
         <path d=\"M0,0 L10,5 L0,10 z\" fill=\"{STROKE}\"/></marker></defs>",
        w = l.width,
        h = l.height,
    );
    let size_of = |v: usize| node_size(&d.nodes[v]);
    for route in &l.routes {
        let edge = &d.edges[route.edge];
        let mut points: Vec<(f32, f32)> = route.items.iter().map(|&i| l.centers[i]).collect();
        let (first, last) = (route.items[0], *route.items.last().unwrap());
        let len = points.len();
        points[0] = clip(d.nodes[first].shape, size_of(first), points[0], points[1]);
        points[len - 1] = clip(d.nodes[last].shape, size_of(last), points[len - 1], points[len - 2]);
        // A label's box sits on its edge: each side ends at the box.
        match route.label.and_then(|item| route.items.iter().position(|&i| i == item)) {
            Some(at) => {
                let gap = label_box(&edge.label);
                let mut before = points[..=at].to_vec();
                let mut after = points[at..].to_vec();
                before[at] = clip(Shape::Rect, gap, points[at], points[at - 1]);
                after[0] = clip(Shape::Rect, gap, points[at], points[at + 1]);
                path(&mut s, &before, edge, false);
                path(&mut s, &after, edge, edge.arrow);
            }
            None => path(&mut s, &points, edge, edge.arrow),
        }
    }
    for (e, edge) in d.edges.iter().enumerate().filter(|(_, e)| e.from == e.to) {
        let (cx, cy) = l.centers[edge.from];
        let (w, h) = size_of(edge.from);
        let horizontal = matches!(d.direction, Direction::Right | Direction::Left);
        let loops = d.edges[..e].iter().filter(|o| o.from == edge.from && o.to == edge.from).count() as f32;
        let reach = LOOP * 0.8 + loops * 10.0;
        let points = if horizontal {
            let y = cy + h / 2.0;
            vec![(cx - w / 4.0, y), (cx, y + reach), (cx + w / 4.0, y)]
        } else {
            let x = cx + w / 2.0;
            vec![(x, cy - h / 4.0), (x + reach, cy), (x, cy + h / 4.0)]
        };
        path(&mut s, &points, edge, edge.arrow);
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
    Ok(Svg { text: s, width: l.width })
}

/// Where the line from `center` toward `toward` leaves the node.
fn clip(shape: Shape, (w, h): (f32, f32), center: (f32, f32), toward: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (toward.0 - center.0, toward.1 - center.1);
    if dx == 0.0 && dy == 0.0 {
        return center;
    }
    let t = match shape {
        Shape::Start | Shape::End => w / 2.0 / (dx * dx + dy * dy).sqrt(),
        Shape::Diamond => 1.0 / (dx.abs() / (w / 2.0) + dy.abs() / (h / 2.0)),
        Shape::Rect | Shape::Round => {
            let tx = if dx == 0.0 { f32::MAX } else { (w / 2.0) / dx.abs() };
            let ty = if dy == 0.0 { f32::MAX } else { (h / 2.0) / dy.abs() };
            tx.min(ty)
        }
    };
    let t = t.min(1.0);
    (center.0 + dx * t, center.1 + dy * t)
}

/// A smooth line from the first point to the last, pulled toward the ones
/// between (a uniform B-spline, as Mermaid draws): no waves where they
/// don't line up.
fn path(s: &mut String, points: &[(f32, f32)], edge: &Edge, arrow: bool) {
    let _ = write!(s, "<path d=\"M{:.1},{:.1}", points[0].0, points[0].1);
    let n = points.len();
    if n == 2 {
        let _ = write!(s, " L{:.1},{:.1}", points[1].0, points[1].1);
    } else {
        let mix = |a: (f32, f32), b: (f32, f32), wa: f32, wb: f32| ((a.0 * wa + b.0 * wb) / (wa + wb), (a.1 * wa + b.1 * wb) / (wa + wb));
        let first = mix(points[0], points[1], 5.0, 1.0);
        let _ = write!(s, " L{:.1},{:.1}", first.0, first.1);
        for i in 2..=n {
            let (p0, p1) = (points[i - 2], points[i - 1]);
            let p = if i < n { points[i] } else { p1 };
            let c1 = mix(p0, p1, 2.0, 1.0);
            let c2 = mix(p0, p1, 1.0, 2.0);
            let end = ((p0.0 + 4.0 * p1.0 + p.0) / 6.0, (p0.1 + 4.0 * p1.1 + p.1) / 6.0);
            let _ = write!(s, " C{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}", c1.0, c1.1, c2.0, c2.1, end.0, end.1);
        }
        let _ = write!(s, " L{:.1},{:.1}", points[n - 1].0, points[n - 1].1);
    }
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
