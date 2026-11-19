//! The viewer's colours, taken from the logo.
//!
//! Colour marks something; it never decorates. Every meaning it carries is
//! also carried by a glyph or a letter, so `NO_COLOR` loses nothing but ease.
//! No background is ever set except the cursor row's, so the viewer takes
//! the terminal's own, light or dark.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Flags, the turn in progress, the touched lines, `⚠`.
    amber: Color,
    /// What the agent changed, and a diff's additions.
    mint: Color,
    /// Deletions, and a diff's removals.
    red: Color,
    /// Reads and the other quiet kinds, and a diff's hunk headers.
    cool: Color,
    /// Directories, ages, frames, the key line.
    dim: Color,
    cursor: Color,
    /// `NO_COLOR`: styles keep their modifiers and lose their colours.
    plain: bool,
}

impl Theme {
    /// The logo's own colours, for terminals that draw 24-bit colour.
    pub const TRUECOLOR: Self = Self {
        amber: Color::Rgb(0xf2, 0xb8, 0x4b),
        mint: Color::Rgb(0x88, 0xeb, 0xb8),
        red: Color::Rgb(0xf0, 0x71, 0x78),
        cool: Color::Rgb(0x9a, 0xa7, 0xb6),
        dim: Color::Rgb(0x6b, 0x76, 0x86),
        cursor: Color::Rgb(0x26, 0x32, 0x47),
        plain: false,
    };

    /// The nearest of the sixteen colours every terminal has. The user's
    /// palette decides the exact shades, which is the point of using them.
    pub const ANSI: Self = Self {
        amber: Color::Yellow,
        mint: Color::Green,
        red: Color::Red,
        cool: Color::Cyan,
        dim: Color::DarkGray,
        cursor: Color::DarkGray,
        plain: false,
    };

    pub const PLAIN: Self = Self {
        plain: true,
        ..Self::ANSI
    };

    /// Chosen from the environment, read through `var` rather than
    /// `std::env` so tests need not set variables (which the workspace
    /// forbids).
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        // no-color.org: set means present and not empty.
        if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::PLAIN;
        }
        match var("COLORTERM").as_deref() {
            Some("truecolor" | "24bit") => Self::TRUECOLOR,
            _ => Self::ANSI,
        }
    }

    fn fg(&self, colour: Color) -> Style {
        if self.plain {
            Style::default()
        } else {
            Style::default().fg(colour)
        }
    }

    pub fn amber(&self) -> Style {
        self.fg(self.amber)
    }

    pub fn mint(&self) -> Style {
        self.fg(self.mint)
    }

    pub fn red(&self) -> Style {
        self.fg(self.red)
    }

    pub fn cool(&self) -> Style {
        self.fg(self.cool)
    }

    pub fn dim(&self) -> Style {
        self.fg(self.dim)
    }

    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    /// The cursor row. Without colour, reverse video: the one row the human
    /// is driving must stay visible.
    pub fn cursor(&self) -> Style {
        if self.plain {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(self.cursor)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_truecolor_terminal_gets_the_logos_own_colours() {
        assert_eq!(
            Theme::from_env(env(&[("COLORTERM", "truecolor")])),
            Theme::TRUECOLOR
        );
        assert_eq!(
            Theme::from_env(env(&[("COLORTERM", "24bit")])),
            Theme::TRUECOLOR
        );
    }

    #[test]
    fn any_other_terminal_gets_its_own_sixteen_colours() {
        assert_eq!(Theme::from_env(env(&[])), Theme::ANSI);
        assert_eq!(Theme::from_env(env(&[("COLORTERM", "yes")])), Theme::ANSI);
    }

    #[test]
    fn no_color_wins_over_everything() {
        let theme = Theme::from_env(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]));
        assert_eq!(theme, Theme::PLAIN);
        assert_eq!(theme.amber(), Style::default());
        assert_eq!(theme.dim(), Style::default());
    }

    #[test]
    fn an_empty_no_color_is_not_set() {
        assert_eq!(Theme::from_env(env(&[("NO_COLOR", "")])), Theme::ANSI);
    }
}
