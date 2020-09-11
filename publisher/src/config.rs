// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use anyhow::{bail, Error};

use structopt::StructOpt;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ChannelConfiguration {
    Mono,
    Stereo,
}

impl std::str::FromStr for ChannelConfiguration {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mono" => Ok(ChannelConfiguration::Mono),
            "stereo" => Ok(ChannelConfiguration::Stereo),
            _ => bail!(
                "Invalid channel configuration '{}', valid values: 'mono', 'stereo'",
                s
            ),
        }
    }
}

#[derive(Debug, StructOpt)]
#[structopt(
    name = "webrtc-audio-publisher",
    about = "Publish an audio stream via WebRTC."
)]
pub struct Config {
    /// Input stream to record, e.g. pulseaudio:default or jack:default.
    #[structopt(short, long)]
    pub input_stream: String,

    /// Server to publish the stream on, e.g. https://localhost:8080
    #[structopt(short, long)]
    pub server: String,
    /// Room to create on the server. If it is published to already then this is an error.
    #[structopt(short = "r", long)]
    pub server_room: String,
    /// Description to use for the room.
    #[structopt(long)]
    pub server_room_description: Option<String>,

    /// TURN server to use, e.g. turn://user:password@foo.bar.com:3478.
    #[structopt(long)]
    pub turn_server: Option<String>,
    /// TURN server to use, e.g. stun://stun.l.google.com:19302.
    #[structopt(long)]
    pub stun_server: Option<String>,

    /// Bitrate to encode the stream to.
    #[structopt(short, long, default_value = "96000")]
    pub bitrate: u32,
    /// Volume to use, 1.0 means no change.
    #[structopt(short, long, default_value = "1.0")]
    pub volume: f64,
    /// Sample rate to convert to.
    #[structopt(long, default_value = "48000")]
    pub sample_rate: u32,
    /// Channel configuration to use, can be mono or stereo.
    #[structopt(long, default_value = "mono")]
    pub channel_configuration: ChannelConfiguration,
}
