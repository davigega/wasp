// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

mod config;

use anyhow::Error;

use structopt::StructOpt;

use config::Config;

use actix_rt::System;

use actix_web::{web, App, HttpResponse, HttpServer, Responder};

async fn index() -> impl Responder {
    HttpResponse::Ok().body("Hello world!")
}

async fn run(cfg: Config) -> Result<(), Error> {
    let server = HttpServer::new(|| App::new().route("/", web::get().to(index)));

    let server = if cfg.use_tls {
        use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};

        let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls())?;
        builder.set_private_key_file(
            cfg.key_file.as_ref().expect("No key file given"),
            SslFiletype::PEM,
        )?;
        builder.set_certificate_chain_file(
            cfg.certificate_file
                .as_ref()
                .expect("No certificate file given"),
        )?;

        server.bind_openssl(format!("0.0.0.0:{}", cfg.port), builder)?
    } else {
        server.bind(format!("0.0.0.0:{}", cfg.port))?
    };

    server.run().await?;

    Ok(())
}

fn main() -> Result<(), Error> {
    let cfg = Config::from_args();

    gst::init()?;

    let env = env_logger::Env::new()
        .filter_or("WEBRTC_AUDIO_SERVER_LOG", "warn")
        .write_style("WEBRTC_AUDIO_SERVER_LOG_STYLE");
    env_logger::init_from_env(env);

    let mut system = System::new("WebRTC Audio Server");
    system.block_on(run(cfg))
}
