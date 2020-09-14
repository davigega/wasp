// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use std::path::PathBuf;

use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "webrtc-audio-server",
    about = "Receives audio streams via WebRTC from publishers and provides them to WebRTC clients."
)]
pub struct Config {
    /// Port to use.
    #[structopt(short, long, default_value = "8080")]
    pub port: u16,

    /// Directory with static HTML/CSS/JS files.
    #[structopt(short, long)]
    pub static_files: PathBuf,

    /// Use TLS.
    #[structopt(short = "t", long)]
    pub use_tls: bool,
    /// Certificate public key file.
    #[structopt(short = "c", long, required_if("use_tls", "true"))]
    pub certificate_file: Option<PathBuf>,
    /// Certificate private key file.
    #[structopt(short = "k", long, required_if("use_tls", "true"))]
    pub key_file: Option<PathBuf>,

    /// TURN server to use, e.g. turn://user:password@foo.bar.com:3478.
    #[structopt(long)]
    pub turn_server: Option<String>,
    /// TURN server to use, e.g. stun://stun.l.google.com:19302.
    #[structopt(long)]
    pub stun_server: Option<String>,
}
