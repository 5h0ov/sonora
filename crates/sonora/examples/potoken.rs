//! Mints a YouTube proof-of-origin token on its own, outside the app, and prints it.
//!
//! This is the same hidden page the app opens when the YouTube client asks for a token: a
//! throwaway browser session on youtube.com running the app's minting script, which leaves the
//! answer in a cookie. It exists to check that this machine's browser engine passes Google's
//! attestation at all, which nothing else can tell you.
//!
//! ```sh
//! cargo run --package sonora --example potoken -- <binding>
//! ```
//!
//! The binding is whatever the token has to be bound to: the session's visitor id, or the
//! account's data sync id when signed in. Any string works for a smoke test, but a token minted
//! for the wrong binding is refused by the stream host.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

const POLL: Duration = Duration::from_millis(250);
const PATIENCE: Duration = Duration::from_secs(60);

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    if !webview::supported() {
        bail!("this platform has no browser engine to mint with");
    }
    let binding = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "CgtTb25vcmFQcm9iZQ".to_string());
    let escaped: String = binding
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect();
    let target = webview::Target {
        url: format!("https://www.youtube.com/?binding={escaped}"),
        landing: "www.youtube.com".to_owned(),
        domain: "youtube.com".to_owned(),
        proof: vec!["SONORA_POT".to_owned()],
        title: String::new(),
        agent: None,
        script: Some(include_str!("../../state/src/potoken.js").to_owned()),
    };

    println!("minting for {binding}");
    let mut page = webview::Page::open(target)?;
    let opened = Instant::now();
    loop {
        match page.poll() {
            webview::Poll::Cookies(header) => {
                let answer = header
                    .split(';')
                    .filter_map(|pair| pair.trim().split_once('='))
                    .find(|(name, _)| *name == "SONORA_POT")
                    .map(|(_, value)| value)
                    .unwrap_or_default();
                match answer.strip_prefix('!') {
                    Some(why) => bail!("the page could not mint a token: {why}"),
                    None => {
                        println!("token: {answer}");
                        return Ok(());
                    }
                }
            }
            webview::Poll::Closed => bail!("the page closed before it answered"),
            webview::Poll::Pending => {}
        }
        if opened.elapsed() >= PATIENCE {
            bail!("the page did not answer in {PATIENCE:?}");
        }
        std::thread::sleep(POLL);
    }
}
