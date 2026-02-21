#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteFocus {
    Input,
    Dropdown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteCommand {
    Projects,
}

impl PaletteCommand {
    pub const ALL: &[PaletteCommand] = &[PaletteCommand::Projects];

    pub fn label(self) -> &'static str {
        match self {
            PaletteCommand::Projects => "projects",
        }
    }

    pub fn filter(query: &str) -> Vec<PaletteCommand> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<PaletteCommand> = Self::ALL
            .iter()
            .filter(|cmd| cmd.label().contains(&query_lower))
            .copied()
            .collect();
        results.sort_by_key(|cmd| cmd.label());
        results
    }
}
