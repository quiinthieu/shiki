use clap::Parser;
use sk_core::product;

#[derive(Parser, Debug)]
#[command(
    name = product::CLI_BIN,
    bin_name = product::CLI_BIN,
    version = product::VERSION,
    about = "Terminal colours that drift slowly with local sunrise, sunset and the seasons"
)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
