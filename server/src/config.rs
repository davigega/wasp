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

    /// Use TLS.
    #[structopt(short = "t", long)]
    pub use_tls: bool,
    /// Certificate public key file.
    #[structopt(short = "c", long, required_if("use_tls", "true"))]
    pub certificate_file: Option<PathBuf>,
    /// Certificate private key file.
    #[structopt(short = "k", long, required_if("use_tls", "true"))]
    pub key_file: Option<PathBuf>,
}
