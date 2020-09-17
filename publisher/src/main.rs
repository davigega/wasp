// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use anyhow::Error;
use structopt::StructOpt;

use log::info;

mod config;
use config::Config;
mod publisher;
use publisher::Publisher;

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

    runtime.block_on(async move {
        let (publisher, join_handle) = Publisher::run(cfg).await?;

        // Stop cleanly on ctrl+c
        let mut publisher_clone = publisher.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Received ctrl+c");
            let _ = publisher_clone.stop();
        });

        join_handle.await
    })
}
