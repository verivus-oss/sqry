use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SortField {
    /// Sort by file path (lexicographic), then line/column, then name
    File,
    /// Sort by line number, then column, then file path
    Line,
    /// Sort by symbol name (lexicographic), then file path, then line
    Name,
    /// Sort by symbol kind (lexicographic), then name, then file path
    Kind,
}
