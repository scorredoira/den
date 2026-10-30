//! Diagrams at their limits, checked on the SVG as drawn: no edge runs
//! through a box or a label that isn't its own, and no two edges run on top
//! of each other.
use super::*;
use std::collections::HashSet;

/// A box of the drawing an edge must keep out of.
enum Obstacle {
    Rect { x: f32, y: f32, w: f32, h: f32 },
    Diamond { cx: f32, cy: f32, hw: f32, hh: f32 },
    Circle { cx: f32, cy: f32, r: f32 },
}

impl Obstacle {
    /// Whether `p` is inside, more than `margin` from the border.
    fn holds(&self, (px, py): (f32, f32), margin: f32) -> bool {
        match *self {
            Obstacle::Rect { x, y, w, h } => px > x + margin && px < x + w - margin && py > y + margin && py < y + h - margin,
            Obstacle::Diamond { cx, cy, hw, hh } => (px - cx).abs() / (hw - margin) + (py - cy).abs() / (hh - margin) < 1.0,
            Obstacle::Circle { cx, cy, r } => (px - cx).hypot(py - cy) < r - margin,
        }
    }
}

fn attr(tag: &str, name: &str) -> Option<f32> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    tag[at..tag[at..].find('"')? + at].parse().ok()
}

fn numbers(text: &str) -> Vec<f32> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect()
}

/// The boxes, labels and circles, and each edge as points along it.
fn drawing(svg: &str) -> (Vec<Obstacle>, Vec<Vec<(f32, f32)>>) {
    let mut shapes = Vec::new();
    let mut edges = Vec::new();
    for tag in svg.split('<').map(|t| t.split('>').next().unwrap_or("")) {
        if tag.starts_with("rect ") {
            shapes.push(Obstacle::Rect { x: attr(tag, "x").unwrap(), y: attr(tag, "y").unwrap(), w: attr(tag, "width").unwrap(), h: attr(tag, "height").unwrap() });
        } else if tag.starts_with("circle ") && !tag.contains("r=\"6\"") {
            shapes.push(Obstacle::Circle { cx: attr(tag, "cx").unwrap(), cy: attr(tag, "cy").unwrap(), r: attr(tag, "r").unwrap() });
        } else if tag.starts_with("path ") && tag.contains(&format!("fill=\"{FILL}\"")) {
            let n = numbers(&tag[tag.find("d=\"").unwrap()..]);
            let xs: Vec<f32> = n.chunks(2).take(12).map(|p| p[0]).collect();
            let ys: Vec<f32> = n.chunks(2).take(12).map(|p| p[1]).collect();
            let (x0, x1) = (xs.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max));
            let (y0, y1) = (ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max));
            shapes.push(Obstacle::Diamond { cx: (x0 + x1) / 2.0, cy: (y0 + y1) / 2.0, hw: (x1 - x0) / 2.0, hh: (y1 - y0) / 2.0 });
        } else if tag.starts_with("path ") && tag.contains("fill=\"none\"") {
            let d = &tag[tag.find("d=\"").unwrap() + 3..];
            let d = &d[..d.find('"').unwrap()];
            let mut points = Vec::new();
            let mut at = (0.0, 0.0);
            let commands = d.chars().filter(char::is_ascii_alphabetic);
            let bodies = d.split(|c: char| c.is_ascii_alphabetic()).skip(1).map(numbers);
            for (command, n) in commands.zip(bodies) {
                match command {
                    'M' => {
                        at = (n[0], n[1]);
                        points.push(at);
                    }
                    // Each curve by 20 points along it.
                    'C' => {
                        let (c1, c2, to) = ((n[0], n[1]), (n[2], n[3]), (n[4], n[5]));
                        for k in 1..=20 {
                            let t = k as f32 / 20.0;
                            let u = 1.0 - t;
                            let b = |a: f32, b: f32, c: f32, d: f32| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
                            points.push((b(at.0, c1.0, c2.0, to.0), b(at.1, c1.1, c2.1, to.1)));
                        }
                        at = to;
                    }
                    'Q' => {
                        let (c, to) = ((n[0], n[1]), (n[2], n[3]));
                        for k in 1..=20 {
                            let t = k as f32 / 20.0;
                            let u = 1.0 - t;
                            let b = |a: f32, b: f32, c: f32| u * u * a + 2.0 * u * t * b + t * t * c;
                            points.push((b(at.0, c.0, to.0), b(at.1, c.1, to.1)));
                        }
                        at = to;
                    }
                    'L' => {
                        let to = (n[0], n[1]);
                        let steps = ((to.0 - at.0).hypot(to.1 - at.1) / 2.0).ceil().max(1.0) as usize;
                        points.extend((1..=steps).map(|k| {
                            let t = k as f32 / steps as f32;
                            (at.0 + (to.0 - at.0) * t, at.1 + (to.1 - at.1) * t)
                        }));
                        at = to;
                    }
                    other => panic!("path command {other}"),
                }
            }
            edges.push(points);
        }
    }
    (shapes, edges)
}

/// What is wrong in the drawing of `source`, in words.
fn faults(source: &str) -> Vec<String> {
    let svg = render(source).unwrap().text(1.0);
    let (shapes, edges) = drawing(&svg);
    let mut faults = Vec::new();
    for (e, points) in edges.iter().enumerate() {
        if let Some(p) = points.iter().find(|&&p| shapes.iter().any(|s| s.holds(p, 1.5))) {
            faults.push(format!("edge {e} runs through a box at {:.0},{:.0}", p.0, p.1));
        }
    }
    // Edges on top of each other: cells of 3px both go through, away from
    // where they start and end.
    const CELL: f32 = 3.0;
    let cells: Vec<HashSet<(i32, i32)>> = edges
        .iter()
        .map(|points| {
            let (first, last) = (points[0], *points.last().unwrap());
            points
                .iter()
                .filter(|p| (p.0 - first.0).hypot(p.1 - first.1) > 24.0 && (p.0 - last.0).hypot(p.1 - last.1) > 24.0)
                .map(|p| ((p.0 / CELL) as i32, (p.1 / CELL) as i32))
                .collect()
        })
        .collect();
    for a in 0..edges.len() {
        for b in a + 1..edges.len() {
            let shared = cells[a].intersection(&cells[b]).count();
            if shared > 10 {
                faults.push(format!("edges {a} and {b} run together for {shared} cells"));
            }
        }
    }
    faults
}

/// A turn of a conversation: a wide fan, with edges back into its root.
const TURN: &str = "stateDiagram-v2
    [*] --> Working: asks
    [*] --> Queued: asks while the conversation is working
    Queued --> Working: the other turn ends
    Working --> Answered: answers
    Working --> Stopped: Stop
    Working --> Full: reaches the conversation's ceiling
    Working --> OutOfCredit: the credit runs out
    Working --> Failed: fails
    Working --> Stuck: the same round refused three times
    Working --> Unfinished: the server stops
    Working --> Asking: reaches an act that asks
    Failed --> Working: Carry on
    Stuck --> Working: Carry on
    Unfinished --> Working: Carry on
    Full --> Working: Continue
    OutOfCredit --> Working: buys credit and asks
    Asking --> Working: Yes
    Asking --> Declined: No
    Asking --> Withdrawn: another message
";

/// A random graph of `nodes`, each with up to three edges, some labeled.
fn random(nodes: usize, mut seed: u32) -> String {
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize
    };
    let mut source = String::from("flowchart TD\n");
    for i in 0..nodes {
        for _ in 0..next() % 4 {
            let label = ["", "|ok|", "|a longer label|"][next() % 3];
            source.push_str(&format!("  N{i} -->{label} N{}\n", next() % nodes));
        }
    }
    source
}

/// Fans, cycles, every direction, long labels and a dense random graph
/// are drawn with no edge over a box or another edge.
#[test]
fn edges_keep_clear_of_boxes_and_each_other() {
    let mut cases: Vec<String> = [
        TURN,
        "stateDiagram-v2\n [*] --> S0\n S0 --> S1: next\n S1 --> S0: reset\n S1 --> S2: next\n S2 --> S0: reset\n S2 --> S3: next\n S3 --> S0: reset\n S3 --> [*]",
        "stateDiagram-v2\n A --> B: forward\n B --> A: backward\n B --> C: forward\n C --> B: backward\n C --> A: wrap\n A --> C: skip",
        "flowchart BT\n A -->|up| B\n A -->|up too| C\n B --> D\n C --> D\n D -.->|back| A",
        "flowchart RL\n A[Start] -->|go| B{Choice}\n B -->|left| C\n B -->|right| D\n C --> E\n D --> E\n E -->|loop back to start| A",
        "flowchart TD\n A{Is it a very long question that wraps?} -->|yes| B{Another one?}\n A -->|no| C((Circle))\n B -->|yes| D([Stadium])\n B -->|no| E(Round)\n C --> F{Last}\n D --> F\n E --> F\n F -->|maybe| A",
    ]
    .map(String::from)
    .to_vec();
    let pairs = ["A", "B", "C", "D", "E", "F"];
    cases.push(format!("flowchart TD\n{}", pairs.iter().flat_map(|a| pairs.iter().filter(move |b| a < *b).map(move |b| format!(" {a} -->|{a}{b}| {b}\n"))).collect::<String>()));
    cases.push(format!("flowchart LR\n{}", (1..=10).map(|i| format!(" Hub -->|route {i}| R{i}[Right {i}]\n R{i} --> Hub\n")).collect::<String>()));
    cases.push(format!("flowchart TD\n{}", (1..=12).map(|i| format!(" Top -->|branch {i}| M{i}\n M{i} --> Bottom\n M{i} -->|back to top {i}| Top\n")).collect::<String>()));
    cases.push(random(60, 7));
    for source in &cases {
        assert_eq!(faults(source), Vec::<String>::new(), "{source}");
    }
}

/// A fan as wide as the turn's has its last states staggered over rows,
/// so it is drawn about a column wide (before, twice that) at full size.
#[test]
fn a_wide_fan_is_staggered_to_about_a_column() {
    let l = layout(&parse(TURN).unwrap());
    assert!(l.width < COLUMN * 1.3, "{}", l.width);
    let rows: HashSet<usize> = (0..l.ranks.len()).filter(|&v| l.sinks[v]).map(|v| l.ranks[v]).collect();
    assert!(rows.len() >= 3, "{rows:?}");
}

/// A big diagram is laid out in a moment: the preview draws it on every
/// edit. (Before, 150 states took minutes.)
#[test]
fn a_big_diagram_is_quick() {
    let source = random(150, 11);
    let start = std::time::Instant::now();
    render(&source).unwrap();
    // Debug builds are some ten times slower than the app.
    assert!(start.elapsed() < std::time::Duration::from_secs(if cfg!(debug_assertions) { 5 } else { 1 }), "{:?}", start.elapsed());
}

/// Odd blocks, and valid ones with characters dropped, doubled or swapped
/// at random: each is drawn or refused with a reason, never a panic.
#[test]
fn fuzz() {
    let seeds = [
        "stateDiagram-v2\n [*] --> A: go\n A --> B\n B --> A: back\n B --> [*]\n state \"Long name\" as A\n",
        "flowchart LR\n A[Start] -->|yes| B{Ok?}\n B -- no --> C((End))\n B -.-> D([D])\n D ==> A\n C --- D\n",
        "graph TD;\n A-->B;\n B-->C;\n %% comment\n C-->A;\n",
    ];
    let odd = [
        "", "\n", "flowchart", "flowchart XY", "stateDiagram-v2\n [*] --> [*]", "stateDiagram-v2\n [*]", "flowchart TD\n A --> A",
        "flowchart TD\n A[", "flowchart TD\n A -->", "flowchart TD\n --> B", "flowchart TD\n A -->|x B", "flowchart TD\n A -- x B",
        "flowchart TD\r\n A --> B\r\n", "flowchart TD\n\tA\t-->\tB", "stateDiagram-v2\n A --> B:", "stateDiagram-v2\n : x",
        "stateDiagram-v2\n state \"x\" as", "stateDiagram-v2\n direction XY", "flowchart TD\n A[<br><br><br>] --> B",
        "flowchart TD\n Á --> 字 --> 🦀", "flowchart TD\n A -->|||| B", "flowchart TD\n A{} --> B()", "flowchart TD\n A ---> B ----> C",
        "flowchart TD\n A -.-x B", "flowchart TD\n A --o B", "stateDiagram\n A --> B --> C", "flowchart TD\n A[\"quoted \\\"inner\\\"\"] --> B",
    ];
    let check = |source: &str| {
        let result = std::panic::catch_unwind(|| {
            let _ = render(source);
        });
        assert!(result.is_ok(), "panics on {source:?}");
    };
    odd.iter().for_each(|s| check(s));
    let pieces = ['-', '>', '|', '[', ']', '{', '}', '(', ')', ':', '"', '\n', ' ', '*', '.', '=', 'é', '<'];
    let mut seed = 0x2545_f491u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize
    };
    for _ in 0..3000 {
        let mut chars: Vec<char> = seeds[next() % seeds.len()].chars().collect();
        for _ in 0..1 + next() % 4 {
            let at = next() % chars.len();
            match next() % 3 {
                0 => {
                    chars.remove(at);
                }
                1 => chars.insert(at, pieces[next() % pieces.len()]),
                _ => chars[at] = pieces[next() % pieces.len()],
            }
        }
        check(&chars.into_iter().collect::<String>());
    }
}

/// A huge diagram is declared small enough for gpui to draw it, however
/// much it is zoomed: at twice its size, within the GPU's largest texture
/// and 128 MB.
#[test]
fn a_huge_diagram_fits_the_gpu() {
    let mut source = String::from("flowchart TD\n");
    for i in 0..120 {
        source.push_str(&format!("  N{i} --> N{}\n  N{i} -->|to {}| N{}\n", i + 1, (i * 7) % 120, (i * 7) % 120));
    }
    let svg = render(&source).unwrap();
    let scale = svg.scale(3.0);
    let text = svg.text(scale);
    let (w, h) = (attr(&text, "width").unwrap(), attr(&text, "height").unwrap());
    let view = numbers(&text[text.find("viewBox").unwrap()..]);
    assert!(view[3] > 8000.0, "the case is past the limit: {view:?}");
    assert!(w.max(h) * 2.0 <= 16384.0 && w * h * 4.0 * 4.0 <= 128e6 * 1.01, "{w}x{h}");
    assert!((w / h - view[2] / view[3]).abs() < 0.01, "keeps its shape");
    assert_eq!((svg.width * scale).round(), w);
}


