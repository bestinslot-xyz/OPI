use super::*;

mod update;

#[derive(Debug, Parser)]
pub(crate) enum IndexSubcommand {
  #[command(about = "Update the index", alias = "run")]
  Update,
}

impl IndexSubcommand {
  pub(crate) fn run(self, settings: Settings) -> SubcommandResult {
    match self {
      Self::Update => update::run(settings),
    }
  }
}
