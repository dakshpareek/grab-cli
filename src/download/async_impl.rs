use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{
    header::{HeaderMap, ACCEPT_RANGES},
    Client, StatusCode,
};
use std::path::PathBuf;
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::mpsc::Sender,
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

async fn is_resumable(client: &Client, url: &str) -> Result<bool> {
    let resp = client.head(url).send().await?.error_for_status()?;
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

async fn download_and_update_progress(
    bar: &ProgressBar,
    file: &mut File,
    response: reqwest::Response,
) -> Result<()> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();

    let mut total_bytes: usize = 0;
    while let Some(item) = stream.next().await {
        let bytes = item?; // Bytes is Sized
        file.write_all(&bytes).await?;
        total_bytes += bytes.len();
        bar.inc(bytes.len() as u64);
    }

    bar.finish();

    if total_bytes == 0 {
        println!("Warning: No data downloaded");
    }

    Ok(())
}

pub async fn download(client: &Client, url: &str) -> Result<()> {
    let initial_filename = filename_from_url(url);
    let path = PathBuf::from(&initial_filename);

    let already = if path.exists() {
        fs::metadata(&path).await?.len()
    } else {
        0
    };

    let resumable = if already != 0 && is_resumable(client, url).await? {
        println!("Resuming...");
        true
    } else {
        if already != 0 {
            println!("Server doesn't support resuming. Restarting download...");
            fs::remove_file(&path).await?;
        }
        false
    };

    let mut request_builder = client.get(url);
    if resumable {
        request_builder = request_builder.header("Range", format!("bytes={}-", already));
    }

    let mut response;
    let mut attempt = 0;
    loop {
        attempt += 1;
        response = request_builder
            .try_clone()
            .unwrap()
            .send()
            .await?
            .error_for_status()?;
        if response.status() == StatusCode::OK && resumable && attempt == 1 {
            // ...
            request_builder = client.get(url);
            continue;
        }
        break;
    }

    let fname = filename_from_headers(response.headers()).unwrap_or_else(|| filename_from_url(url));
    let path = PathBuf::from(&fname);

    let mut file = if resumable {
        OpenOptions::new().append(true).open(&path).await?
    } else {
        File::create(&path).await?
    };

    let total_length = response
        .content_length()
        .map(|len| if resumable { already + len } else { len });
    let bar = set_progress_bar(total_length);
    if resumable {
        bar.set_position(already);
    }

    download_and_update_progress(&bar, &mut file, response).await?;

    Ok(())
}

pub async fn download_with_progress(
    client: &Client,
    url: &str,
    id: usize,
    progress: Sender<ProgressBar>,
) -> Result<()> {
    // send Started
    progress
        .send(ProgressBar::Started {
            id,
            total: resp.content_length(),
        })
        .await
        .ok();

    // inside loop after bytes.len():
    progress
        .send(ProgressMsg::Progress {
            id,
            delta: bytes.len() as u64,
        })
        .await
        .ok();

    // on finish
    progress.send(ProgressMsg::Finished { id }).await.ok();
    Ok(())
}
