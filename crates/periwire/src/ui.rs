//! Small terminal helpers: colour (respects NO_COLOR and pipes), tables, human-readable numbers.
use std::io::IsTerminal;

pub fn color_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}
fn paint(code: &str, s: &str) -> String {
    if color_on() { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}
pub fn bold(s: &str) -> String { paint("1", s) }
pub fn dim(s: &str) -> String { paint("2", s) }
pub fn green(s: &str) -> String { paint("32", s) }
pub fn yellow(s: &str) -> String { paint("33", s) }
pub fn red(s: &str) -> String { paint("31", s) }
pub fn cyan(s: &str) -> String { paint("36", s) }

pub fn bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

pub fn dur(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s if s < 86400 => format!("{}h {}m", s / 3600, (s / 60) % 60),
        s => format!("{}d {}h", s / 86400, (s / 3600) % 24),
    }
}

pub fn short_fp(fp: &str) -> String {
    if fp.len() > 16 { format!("{}…{}", &fp[..8], &fp[fp.len() - 8..]) } else { fp.to_string() }
}

fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc { if c == 'm' { esc = false; } } else if c == '\x1b' { esc = true; } else { n += 1; }
    }
    n
}

/// Print rows as an aligned table with a dim header.
pub fn table(header: &[&str], rows: &[Vec<String>]) {
    let mut w: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(visible_len(c));
        }
    }
    let line = |cells: Vec<String>, head: bool| {
        let mut out = String::from("  ");
        for (i, c) in cells.iter().enumerate() {
            let pad = w[i].saturating_sub(visible_len(c));
            let c2 = if head { dim(c) } else { c.clone() };
            out.push_str(&c2);
            if i + 1 < cells.len() { out.push_str(&" ".repeat(pad + 2)); }
        }
        println!("{out}");
    };
    line(header.iter().map(|h| h.to_uppercase()).collect(), true);
    for r in rows { line(r.clone(), false); }
}

pub fn hms(t: u64) -> String {
    format!("{:02}:{:02}:{:02}", (t / 3600) % 24, (t / 60) % 60, t % 60)
}
