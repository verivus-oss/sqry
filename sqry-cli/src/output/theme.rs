use clap::ValueEnum;
use colored::{Color, Colorize};

/// Built-in theme selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ThemeName {
    Default,
    Dark,
    Light,
    None,
}

/// Simple style descriptor for terminal output.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub color: Option<Color>,
    pub bold: bool,
    pub dimmed: bool,
}

impl Style {
    #[must_use]
    pub fn new(color: Option<Color>, bold: bool, dimmed: bool) -> Self {
        Self {
            color,
            bold,
            dimmed,
        }
    }

    #[must_use]
    pub fn apply(self, text: &str, use_color: bool) -> String {
        if !use_color {
            return text.to_string();
        }

        let mut styled = text.normal();
        if let Some(color) = self.color {
            styled = styled.color(color);
        }
        if self.bold {
            styled = styled.bold();
        }
        if self.dimmed {
            styled = styled.dimmed();
        }
        styled.to_string()
    }
}

/// Palette of semantic roles for text output.
#[derive(Clone, Debug)]
pub struct Palette {
    pub path: Style,
    pub location: Style,
    pub kind: Style,
    pub name: Style,
    pub repo_label: Style,
    pub repo_name: Style,
    pub dimmed: Style,
}

impl Palette {
    #[must_use]
    pub fn built_in(theme: ThemeName) -> Self {
        match theme {
            ThemeName::Default => Self {
                path: Style::new(Some(Color::Green), false, false),
                location: Style::new(Some(Color::Blue), false, false),
                kind: Style::new(Some(Color::Yellow), false, false),
                name: Style::new(None, true, false),
                repo_label: Style::new(Some(Color::Magenta), false, false),
                repo_name: Style::new(None, true, false),
                dimmed: Style::new(None, false, true),
            },
            ThemeName::Dark => Self {
                path: Style::new(Some(Color::BrightGreen), false, false),
                location: Style::new(Some(Color::BrightBlue), false, false),
                kind: Style::new(Some(Color::BrightYellow), false, false),
                name: Style::new(Some(Color::White), true, false),
                repo_label: Style::new(Some(Color::BrightMagenta), false, false),
                repo_name: Style::new(Some(Color::White), true, false),
                dimmed: Style::new(Some(Color::White), false, true),
            },
            ThemeName::Light => Self {
                path: Style::new(Some(Color::Cyan), false, false),
                location: Style::new(Some(Color::Blue), false, false),
                kind: Style::new(Some(Color::Magenta), false, false),
                name: Style::new(Some(Color::Black), true, false),
                repo_label: Style::new(Some(Color::Magenta), false, false),
                repo_name: Style::new(Some(Color::Black), true, false),
                dimmed: Style::new(Some(Color::Black), false, true),
            },
            ThemeName::None => Self {
                path: Style::new(None, false, false),
                location: Style::new(None, false, false),
                kind: Style::new(None, false, false),
                name: Style::new(None, false, false),
                repo_label: Style::new(None, false, false),
                repo_name: Style::new(None, false, false),
                dimmed: Style::new(None, false, true),
            },
        }
    }
}
