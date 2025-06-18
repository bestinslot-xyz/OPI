use super::*;

pub mod index;

use crate::index::get_tx_limits;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
  #[command(subcommand, about = "Index commands")]
  Index(index::IndexSubcommand),
  #[command(about = "List max transfer counts")]
  MaxTransferCounts,
}

fn max_transfer_counts() -> SubcommandResult {
  // create a dictionary. set 'default' to 2 and 'brc20-approve-conditional' to 5
  let max_transfer_counts = get_tx_limits();
  Ok(Some(Box::new(max_transfer_counts)))
}

impl Subcommand {
  pub(crate) fn run(self, settings: Settings) -> SubcommandResult {
    match self {
      Self::Index(index) => index.run(settings),
      Self::MaxTransferCounts => max_transfer_counts(),
    }
  }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum OutputFormat {
  #[default]
  Json,
  Yaml,
  Minify,
}

pub trait Output: Send {
  fn print(&self, format: OutputFormat);
}

impl<T> Output for T
where
  T: Serialize + Send,
{
  fn print(&self, format: OutputFormat) {
    match format {
      OutputFormat::Json => serde_json::to_writer_pretty(io::stdout(), self).ok(),
      OutputFormat::Yaml => serde_yaml::to_writer(io::stdout(), self).ok(),
      OutputFormat::Minify => serde_json::to_writer(io::stdout(), self).ok(),
    };
    println!();
  }
}

pub(crate) type SubcommandResult = Result<Option<Box<dyn Output>>>;
