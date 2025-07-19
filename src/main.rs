use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::File;
use std::io::Read;
use std::io::{self, Write};

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    url: String,
}

fn get_filename(response: &reqwest::blocking::Response) -> String {
    if let Some(cd_val) = response.headers().get(reqwest::header::CONTENT_DISPOSITION) {
        if let Ok(disposition) = cd_val.to_str() {
            if let Some(fname) = disposition
                .split(';')
                .nth(1)
                .and_then(|s| s.split('=').nth(1))
            {
                let fname = fname.trim_matches('"');
                if !fname.is_empty() {
                    return fname.to_string();
                }
            }
        }
    }
    // fallback: derive from URL
    response
        .url()
        .path_segments()
        .and_then(|segments| segments.last())
        .unwrap_or("download.bin")
        .to_string()
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // let url = "https://freetestdata.com/wp-content/uploads/2024/01/sample-zip.rar";

    let url = &cli.url;

    let client = reqwest::blocking::Client::builder()
        .user_agent("dlm-head") // Sets the default User-Agent header for all requests
        .build()?; // Builds the client; ? propagates any config errors

    let mut response = client.get(url).send()?.error_for_status()?; // If we reach here, status is 2xx success
    let content_length = response.content_length();
    let bar = ProgressBar::new(content_length.unwrap_or(0));
    bar.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"),
    );

    let mut buffer = [0u8; 64000];
    let mut total_bytes: usize = 0;

    let filename = get_filename(&response);

    let mut file = File::create(&filename)?;
    loop {
        let bytes_read = response.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        total_bytes += bytes_read;
        bar.inc(bytes_read as u64);
        // Improved progress: In-place update
        print!("\rProcessed: {total_bytes} bytes");
        io::stdout().flush().unwrap();
    }
    bar.finish();

    if total_bytes as u64 != content_length.unwrap_or(0) {
        println!("Warning: Downloaded size does not match Content-Length header");
    }

    if total_bytes == 0 {
        println!("Warning: No data downloaded");
    }

    // println!(
    //     "\nDownload complete {}! Total size: {} bytes",
    //     filename, total_bytes
    // );

    Ok(())
}
