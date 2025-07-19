use clap::Parser;
mod download;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    url: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let url = &cli.url;

    let client = reqwest::blocking::Client::builder()
        .user_agent("grab-cli") // Sets the default User-Agent header for all requests
        .build()?; // Builds the client; ? propagates any config errors

    download::blocking::download(&client, url)?;
    Ok(())
}
