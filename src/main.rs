use std::io::Read;
use std::io::{self, Write};

fn main() -> anyhow::Result<()> {
    let url = "https://freetestdata.com/wp-content/uploads/2024/01/sample-zip.rar";

    let client = reqwest::blocking::Client::builder()
        .user_agent("dlm-head") // Sets the default User-Agent header for all requests
        .build()?; // Builds the client; ? propagates any config errors

    let mut response = client.get(url).send()?.error_for_status()?; // If we reach here, status is 2xx success

    println!("Success. Response Headers:");
    if let Some(len) = response.content_length() {
        println!("Content-Length: {len}");
    }

    let mut buffer = [0u8; 8192];
    let mut total_bytes: usize = 0;

    loop {
        let bytes_read = response.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        total_bytes += bytes_read;
        // Improved progress: In-place update
        print!("\rProcessed: {total_bytes} bytes");
        io::stdout().flush().unwrap();
    }
    println!("\nDownload complete! Total size: {} bytes", total_bytes);

    Ok(())
}
