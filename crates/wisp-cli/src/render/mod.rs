//! Terminal output.

pub mod doctor;

pub use doctor::doctor;

/// Output assembled a line at a time.
#[derive(Default)]
struct Out(String);

impl Out {
    fn line(&mut self, args: std::fmt::Arguments<'_>) {
        self.0.push_str(&args.to_string());
        self.0.push('\n');
    }
}

pub struct Style {
    pub color: bool,
}

impl Style {
    pub fn new(color: bool) -> Self {
        Style { color }
    }
    pub fn dim(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn bold(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn green(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[32m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn yellow(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[33m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn red(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[31m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
}

pub fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let mut s = String::with_capacity(width);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('·');
    }
    s
}

pub fn bytes_human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_proportional_and_bounded() {
        assert_eq!(bar(0.0, 4), "····", "empty");
        assert_eq!(bar(1.0, 4), "████", "full");
        assert_eq!(bar(0.5, 4), "██··", "half");
        assert_eq!(bar(9.0, 4), "████", "fractions above 1 clamp");
        assert_eq!(bar(-1.0, 4), "····", "fractions below 0 clamp");
    }

    #[test]
    fn byte_sizes_read_like_sizes() {
        assert_eq!(bytes_human(512), "512 B", "bytes stay bytes");
        assert_eq!(bytes_human(2048), "2.0 KB", "kilobytes");
        assert_eq!(bytes_human(5 << 20), "5.0 MB", "megabytes");
    }

    #[test]
    fn style_is_plain_when_colour_is_off() {
        let st = Style::new(false);
        assert_eq!(st.red("x"), "x", "no escape codes without colour");
        assert_eq!(st.bold("x"), "x", "no escape codes without colour");
        assert!(
            Style::new(true).red("x").contains('\x1b'),
            "escape codes with colour"
        );
    }
}
