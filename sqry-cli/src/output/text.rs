//! Text output formatter with optional colors
//!
//! Uses `DisplaySymbol` directly without deprecated Symbol type.

use super::{
    DisplaySymbol, Formatter, GroupedContext, MatchLocation, NameDisplayMode, OutputStreams,
    Palette, PreviewConfig, PreviewExtractor, ThemeName,
};
use anyhow::Result;
use sqry_core::workspace::NodeWithRepo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_ALIGN_WIDTH: usize = 80;

type MatchMap<'a> = HashMap<(PathBuf, usize), Vec<&'a DisplaySymbol>>;

/// Text formatter for human-readable output
pub struct TextFormatter {
    use_color: bool,
    display_mode: NameDisplayMode,
    palette: Palette,
    preview_config: Option<PreviewConfig>,
    workspace_root: PathBuf,
}

impl TextFormatter {
    /// Create new text formatter
    #[must_use]
    pub fn new(use_color: bool, display_mode: NameDisplayMode, theme: ThemeName) -> Self {
        // Respect NO_COLOR environment variable (handled by caller) and theme=none
        let use_color = use_color && theme != ThemeName::None && std::env::var("NO_COLOR").is_err();

        if !use_color {
            colored::control::set_override(false);
        }

        Self {
            use_color,
            display_mode,
            palette: Palette::built_in(theme),
            preview_config: None,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Enable preview rendering with the given configuration and workspace root
    #[must_use]
    pub fn with_preview(mut self, config: PreviewConfig, workspace_root: PathBuf) -> Self {
        self.preview_config = Some(config);
        self.workspace_root = workspace_root;
        self
    }

    /// Format file path with color
    #[allow(dead_code)]
    fn format_path(&self, path: &std::path::Path) -> String {
        let path_str = path.display().to_string();
        self.palette.path.apply(&path_str, self.use_color)
    }

    /// Format line:column with color
    fn format_location(&self, line: usize, column: usize) -> String {
        let loc = format!("{line}:{column}");
        self.palette.location.apply(&loc, self.use_color)
    }

    /// Format symbol kind with color
    fn format_kind(&self, display: &DisplaySymbol) -> String {
        self.palette
            .kind
            .apply(display.kind_string(), self.use_color)
    }

    /// Format symbol name (bold if color enabled)
    fn format_name(&self, name: &str) -> String {
        self.palette.name.apply(name, self.use_color)
    }

    fn format_path_str(&self, path: &str) -> String {
        self.palette.path.apply(path, self.use_color)
    }

    /// Shorten a long string by keeping prefix/suffix and inserting ellipsis in the middle.
    fn shorten_middle(s: &str, max_len: usize) -> String {
        if s.chars().count() <= max_len || max_len < 5 {
            return s.to_string();
        }
        let ellipsis = "...";
        let keep = (max_len.saturating_sub(ellipsis.len())) / 2;
        let prefix: String = s.chars().take(keep).collect();
        let suffix: String = s
            .chars()
            .rev()
            .take(keep)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("{prefix}{ellipsis}{suffix}")
    }

    /// Format workspace results that include repository metadata.
    /// P2-3 Step 2e: Text formatting is display/logging code - deprecated accessors allowed
    ///
    /// # Errors
    /// Returns an error if writing to the output streams fails.
    #[allow(deprecated)]
    pub fn format_workspace(
        &self,
        symbols: &[NodeWithRepo],
        streams: &mut OutputStreams,
    ) -> Result<()> {
        if symbols.is_empty() {
            let msg = self
                .palette
                .dimmed
                .apply("No workspace matches", self.use_color);
            streams.write_diagnostic(&msg)?;
            return Ok(());
        }

        let mut align_width = 0;
        let mut formatted: Vec<(String, usize, String)> = Vec::with_capacity(symbols.len());

        for entry in symbols {
            let info = &entry.match_info;
            let repo_segment = format!(
                "{} {}",
                self.palette.repo_label.apply("repo", self.use_color),
                self.palette
                    .repo_name
                    .apply(entry.repo_name.as_str(), self.use_color)
            );

            let display_name = self.format_name(&info.name);

            let loc_raw =
                self.format_location(info.start_line as usize, info.start_column as usize);
            let path_budget = MAX_ALIGN_WIDTH.saturating_sub(loc_raw.chars().count() + 1);
            let path_raw = Self::shorten_middle(&info.file_path.display().to_string(), path_budget);
            let path_colored = self.format_path_str(&path_raw);
            let loc_colored = self.palette.location.apply(&loc_raw, self.use_color);
            let path_loc_raw = format!("{path_raw}:{loc_raw}");
            let path_loc_colored = format!("{path_colored}:{loc_colored}");
            let width = path_loc_raw.chars().count();
            align_width = align_width.max(width);

            let kind_str = info.kind.as_str();
            let kind_colored = self.palette.kind.apply(kind_str, self.use_color);
            let tail = format!("{path_loc_colored} {kind_colored} {display_name}");

            formatted.push((repo_segment, width, tail));
        }

        align_width = align_width.min(MAX_ALIGN_WIDTH);

        for (repo_segment, raw_width, tail) in formatted {
            let pad = align_width.saturating_sub(raw_width);
            let line = format!(
                "{repo_segment} {tail:>width$}",
                tail = tail,
                width = tail.len() + pad
            );
            streams.write_result(&line)?;
        }

        let summary = format!(
            "\n{} workspace matches",
            self.palette
                .name
                .apply(&symbols.len().to_string(), self.use_color)
        );
        streams.write_diagnostic(&summary)?;
        Ok(())
    }
}

impl Formatter for TextFormatter {
    fn format(
        &self,
        symbols: &[DisplaySymbol],
        _metadata: Option<&super::FormatterMetadata>,
        streams: &mut super::OutputStreams,
    ) -> Result<()> {
        if symbols.is_empty() {
            let msg = self
                .palette
                .dimmed
                .apply("No matches found", self.use_color);
            streams.write_diagnostic(&msg)?;
            return Ok(());
        }

        let mut preview_extractor = self
            .preview_config
            .as_ref()
            .map(|config| PreviewExtractor::new(config.clone(), self.workspace_root.clone()));

        let mut align_width = 0;
        let mut formatted: Vec<(String, usize, String, String)> = Vec::with_capacity(symbols.len());

        for display in symbols {
            let loc_raw = self.format_location(display.start_line, display.start_column);
            let path_budget = MAX_ALIGN_WIDTH.saturating_sub(loc_raw.chars().count() + 1);
            let path_raw =
                Self::shorten_middle(&display.file_path.display().to_string(), path_budget);
            let path_colored = self.format_path_str(&path_raw);
            let loc_colored = self.palette.location.apply(&loc_raw, self.use_color);
            let path_loc_raw = format!("{path_raw}:{loc_raw}");
            let path_loc_colored = format!("{path_colored}:{loc_colored}");
            let width = path_loc_raw.chars().count();
            align_width = align_width.max(width);
            formatted.push((
                path_loc_colored,
                width,
                self.format_kind(display),
                self.format_display_name(display),
            ));
        }

        align_width = align_width.min(MAX_ALIGN_WIDTH);

        for (path_loc, raw_width, kind, name) in formatted {
            let pad = align_width.saturating_sub(raw_width);
            let line = format!("{path_loc}{:pad$} {kind} {name}", "", pad = pad);
            streams.write_result(&line)?;
        }

        if let Some(ref mut extractor) = preview_extractor {
            self.write_grouped_previews(symbols, extractor, streams)?;
        }

        // Summary to stderr
        let summary = format!(
            "\n{} matches found",
            self.palette
                .name
                .apply(&symbols.len().to_string(), self.use_color)
        );
        streams.write_diagnostic(&summary)?;

        Ok(())
    }
}

impl TextFormatter {
    fn format_display_name(&self, display: &DisplaySymbol) -> String {
        let simple = &display.name;

        match self.display_mode {
            NameDisplayMode::Simple => self.format_name(simple),
            NameDisplayMode::Qualified => {
                let qualified_opt = display
                    .caller_identity
                    .as_ref()
                    .or(display.callee_identity.as_ref())
                    .map(|identity| identity.qualified.as_str())
                    .filter(|q| !q.is_empty())
                    .or({
                        if display.qualified_name.is_empty() {
                            None
                        } else {
                            Some(display.qualified_name.as_str())
                        }
                    });

                if let Some(qualified) = qualified_opt {
                    if qualified == simple {
                        self.format_name(qualified)
                    } else {
                        format!(
                            "{} ({})",
                            self.format_name(qualified),
                            self.format_name(simple)
                        )
                    }
                } else {
                    self.format_name(simple)
                }
            }
        }
    }

    fn write_grouped_previews(
        &self,
        symbols: &[DisplaySymbol],
        extractor: &mut PreviewExtractor,
        streams: &mut OutputStreams,
    ) -> Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }

        let (matches, match_map) = Self::build_match_context(symbols);
        let mut grouped = extractor.extract_grouped(&matches);
        Self::sort_grouped_contexts(&mut grouped);
        let gutter_width = Self::compute_gutter_width(&grouped);

        if !grouped.is_empty() {
            streams.write_result("")?;
        }

        for group in &grouped {
            self.write_grouped_preview_group(group, &match_map, gutter_width, streams)?;
        }

        Ok(())
    }
}

impl TextFormatter {
    fn build_match_context<'a>(symbols: &'a [DisplaySymbol]) -> (Vec<MatchLocation>, MatchMap<'a>) {
        let mut matches = Vec::with_capacity(symbols.len());
        let mut match_map: MatchMap<'a> = HashMap::new();

        for display in symbols {
            let file = display.file_path.clone();
            matches.push(MatchLocation {
                file: file.clone(),
                line: display.start_line,
            });
            match_map
                .entry((file, display.start_line))
                .or_default()
                .push(display);
        }

        (matches, match_map)
    }

    fn sort_grouped_contexts(grouped: &mut [GroupedContext]) {
        grouped.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.end_line.cmp(&b.end_line))
        });
    }

    fn compute_gutter_width(grouped: &[GroupedContext]) -> usize {
        grouped
            .iter()
            .flat_map(|g| g.lines.iter().map(|l| l.line_number.to_string().len()))
            .max()
            .unwrap_or(1)
    }

    fn write_grouped_preview_group(
        &self,
        group: &GroupedContext,
        match_map: &MatchMap<'_>,
        gutter_width: usize,
        streams: &mut OutputStreams,
    ) -> Result<()> {
        let file_fmt = self.format_group_file(group);

        if let Some(err) = &group.error {
            streams.write_result(&format!("{file_fmt}: {err}"))?;
            streams.write_result("")?;
            return Ok(());
        }

        streams.write_result(&format!(
            "{file_fmt}: lines {}-{}",
            group.start_line, group.end_line
        ))?;

        for line in &group.lines {
            let marker = self.group_line_marker(line.is_match);
            let gutter = format!("{:>width$}", line.line_number, width = gutter_width);
            let content =
                self.decorate_grouped_line(&group.file, line.line_number, &line.content, match_map);
            streams.write_result(&format!("{marker} {gutter} | {content}"))?;
        }

        streams.write_result("")?;
        Ok(())
    }

    fn format_group_file(&self, group: &GroupedContext) -> String {
        let file_str = group.file.display().to_string();
        self.palette.path.apply(&file_str, self.use_color)
    }

    fn group_line_marker(&self, is_match: bool) -> String {
        if is_match {
            self.palette.name.apply(">", self.use_color)
        } else {
            " ".to_string()
        }
    }

    fn decorate_grouped_line(
        &self,
        file: &Path,
        line_number: usize,
        content: &str,
        match_map: &MatchMap<'_>,
    ) -> String {
        let mut content = content.to_string();
        if let Some(symbols_at_line) = match_map.get(&(file.to_path_buf(), line_number))
            && let Some(annotations) = self.build_line_annotation(symbols_at_line)
        {
            content = format!("{content}  // {annotations}");
        }
        content
    }

    fn build_line_annotation(&self, symbols_at_line: &[&DisplaySymbol]) -> Option<String> {
        let annotations: Vec<String> = symbols_at_line
            .iter()
            .map(|d| format!("{} {}", d.kind_string(), self.format_display_name(d)))
            .collect();
        (!annotations.is_empty()).then(|| annotations.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::output::TestOutputStreams;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_display_symbol(name: &str, kind: &str, path: PathBuf, line: usize) -> DisplaySymbol {
        DisplaySymbol {
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: kind.to_string(),
            file_path: path,
            start_line: line,
            start_column: 1,
            end_line: line,
            end_column: 5,
            metadata: HashMap::new(),
            caller_identity: None,
            callee_identity: None,
        }
    }

    #[test]
    fn test_text_formatter_no_color() {
        let formatter = TextFormatter::new(false, NameDisplayMode::Simple, ThemeName::Default);
        assert!(!formatter.use_color);

        let path = formatter.format_path(&PathBuf::from("test.rs"));
        assert_eq!(path, "test.rs");

        let loc = formatter.format_location(10, 5);
        assert_eq!(loc, "10:5");

        let name = formatter.format_name("main");
        assert_eq!(name, "main");
    }

    #[test]
    fn test_text_formatter_respects_no_color_env() {
        unsafe {
            std::env::set_var("NO_COLOR", "1");
        }
        let formatter = TextFormatter::new(true, NameDisplayMode::Simple, ThemeName::Default);
        assert!(!formatter.use_color);
        unsafe {
            std::env::remove_var("NO_COLOR");
        }
    }

    #[test]
    fn test_text_formatter_none_theme_disables_color() {
        let formatter = TextFormatter::new(true, NameDisplayMode::Simple, ThemeName::None);
        assert!(!formatter.use_color);
        let path = formatter.format_path(&PathBuf::from("file.rs"));
        assert_eq!(path, "file.rs");
    }

    #[test]
    fn test_shorten_middle() {
        let s = "this/is/a/very/long/path.rs";
        let shortened = TextFormatter::shorten_middle(s, 10);
        assert!(shortened.len() <= 13);
        assert!(shortened.contains("..."));
    }

    #[test]
    fn test_text_formatter_with_preview_grouped() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sample.rs");
        fs::write(&path, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();

        let sym1 = make_display_symbol("a", "function", path.clone(), 1);
        let sym2 = make_display_symbol("b", "function", path.clone(), 2);

        let formatter = TextFormatter::new(false, NameDisplayMode::Simple, ThemeName::Default)
            .with_preview(PreviewConfig::new(1), tmp.path().to_path_buf());
        let (test, mut streams) = TestOutputStreams::new();

        formatter.format(&[sym1, sym2], None, &mut streams).unwrap();

        let out = test.stdout_string();
        assert!(out.contains("lines 1-3"), "preview header missing: {out}");
        assert!(
            out.contains("> 1 | fn a() {}") && out.contains("> 2 | fn b() {}"),
            "match markers missing: {out}"
        );
    }

    #[test]
    fn test_text_formatter_preview_missing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.rs");

        let sym = make_display_symbol("missing", "function", path, 1);

        let formatter = TextFormatter::new(false, NameDisplayMode::Simple, ThemeName::Default)
            .with_preview(PreviewConfig::new(1), tmp.path().to_path_buf());
        let (test, mut streams) = TestOutputStreams::new();

        formatter.format(&[sym], None, &mut streams).unwrap();

        let out = test.stdout_string();
        assert!(
            out.contains("[file not found"),
            "expected error preview: {out}"
        );
    }
}
