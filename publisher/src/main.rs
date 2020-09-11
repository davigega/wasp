// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use anyhow::Error;
use structopt::StructOpt;

mod config;
use config::Config;

async fn run(_cfg: &Config) -> Result<(), Error> {
    Ok(())
}

fn main() -> Result<(), Error> {
    let cfg = Config::from_args();

    gst::init()?;

    let env = env_logger::Env::new()
        .filter_or("WEBRTC_AUDIO_PUBLISHER_LOG", "warn")
        .write_style("WEBRTC_AUDIO_PUBLISHER_LOG_STYLE");
    env_logger::init_from_env(env);

    let mut runtime = tokio::runtime::Builder::new()
        .basic_scheduler()
        .enable_all()
        .build()?;

    runtime.block_on(run(&cfg))
}
