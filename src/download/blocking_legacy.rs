use std::fs;
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Ok, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, ACCEPT_RANGES},
};

fn filename_from_url(url: &str) -> String {
    url.split("/").last().unwrap_or("download.bin").to_string()
}

fn filename_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|cd_val| cd_val.to_str().ok())
        .and_then(|disposition| {
            disposition.split(';').find_map(|part| {
                let part = part.trim();
                if let Some(filename) = part.strip_prefix("filename=") {
                    let filename = filename.trim_matches('"');
                    if !filename.is_empty() {
                        return Some(filename.to_string());
                    }
                }
                None
            })
        })
}

fn is_resumable(client: &Client, url: &str) -> Result<bool> {
    let resp = client.head(url).send()?.error_for_status()?;
    let resumable = resp
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);
    Ok(resumable)
}

fn set_progress_bar(total_length: Option<u64>) -> ProgressBar {
    match total_length {
        Some(length) => {
            let bar = ProgressBar::new(length);
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .expect("Progress bar template should be valid")
                    .progress_chars("#>-"),
            );
            bar
        }
        None => {
            let bar = ProgressBar::new_spinner();
            bar.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.green} [{elapsed_precise}] {bytes} downloaded")
                    .expect("Spinner template should be valid"),
            );
            bar.enable_steady_tick(std::time::Duration::from_millis(100));
            bar
        }
    }
}

fn download_and_update_progress(
    bar: &ProgressBar,
    file: &mut File,
    response: &mut reqwest::blocking::Response,
) -> Result<()> {
    let mut buffer = [0u8; 64000];
    let mut total_bytes: usize = 0;

    loop {
        let bytes_read = response.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        total_bytes += bytes_read;
        bar.inc(bytes_read as u64);
    }
    bar.finish();

    // if total_bytes as u64 != content_length.unwrap_or(0) {
    //     println!("Warning: Downloaded size does not match Content-Length header");
    // }

    if total_bytes == 0 {
        println!("Warning: No data downloaded");
    }

    Ok(())
}

pub fn download(client: &Client, url: &str) -> Result<()> {
    // First, determine our initial filename from URL
    let initial_filename = filename_from_url(url);
    let mut path = PathBuf::from(&initial_filename);

    let already = if path.exists() {
        fs::metadata(&path)?.len()
    } else {
        0
    };

    let mut request_builder = client.get(url);

    let resumable = if already != 0 && is_resumable(client, url)? {
        request_builder = request_builder.header("Range", format!("bytes={}-", already));
        println!("Resuming...");
        true
    } else {
        if already != 0 {
            println!("Server doesn't support resuming. Restarting download...");
            fs::remove_file(&path)?;
        }
        false
    };

    let mut response = request_builder.send()?.error_for_status()?; // If we reach here, status is 2xx success
    let status = response.status();
    if resumable && status != reqwest::StatusCode::PARTIAL_CONTENT {
        println!("Expected 206 but got {status}; restarting full download");
        drop(response); // release body
        fs::remove_file(&path)?;
        return download(client, url); // recursive fresh start
    }

    // Naming logic: Filename from headers has priority over URL-based name
    let fname = filename_from_headers(response.headers()).unwrap_or(initial_filename);
    path = PathBuf::from(&fname);

    // Decide if we should append to existing file or create a new one
    let mut file = if resumable {
        fs::OpenOptions::new().append(true).open(&path)?
    } else {
        File::create(&path)?
    };

    // Adjust total content length appropriately (for progress bar)
    let total_length = match response.content_length() {
        Some(len) if resumable => Some(already + len), // total length = already downloaded + remaining
        Some(len) => Some(len),
        None => None,
    };

    let bar = set_progress_bar(total_length);
    if resumable {
        bar.set_position(already);
    }
    download_and_update_progress(&bar, &mut file, &mut response)?;

    Ok(())
}
