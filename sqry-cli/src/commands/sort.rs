use crate::args::SortField;
use crate::output::DisplaySymbol;

pub fn sort_symbols(symbols: &mut [DisplaySymbol], sort_field: SortField) {
    symbols.sort_by(|a, b| match sort_field {
        SortField::File => a
            .file_path
            .cmp(&b.file_path)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.start_column.cmp(&b.start_column))
            .then_with(|| a.name.cmp(&b.name)),
        SortField::Line => a
            .start_line
            .cmp(&b.start_line)
            .then_with(|| a.start_column.cmp(&b.start_column))
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.name.cmp(&b.name)),
        SortField::Name => a
            .name
            .cmp(&b.name)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.start_line.cmp(&b.start_line)),
        SortField::Kind => a
            .kind
            .cmp(&b.kind)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.file_path.cmp(&b.file_path)),
    });
}
