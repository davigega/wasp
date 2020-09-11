// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;

use actix_files::NamedFile;
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer, Responder};

async fn index(cfg: web::Data<Config>) -> Result<NamedFile, actix_web::Error> {
    let full_path = cfg.static_files.join("index.html");

    let file = NamedFile::open(full_path)?;

    Ok(file.use_last_modified(true))
}

async fn ws(_cfg: web::Data<Config>) -> impl Responder {
    HttpResponse::Ok().body("Hello world!")
}

async fn static_file(
    cfg: web::Data<Config>,
    req: HttpRequest,
) -> Result<NamedFile, actix_web::Error> {
    let path: std::path::PathBuf = req.match_info().query("filename").parse().unwrap();
    let full_path = cfg.static_files.join(path);

    let file = NamedFile::open(full_path)?;

    Ok(file.use_last_modified(true))
}

pub async fn run(cfg: Config) -> Result<(), anyhow::Error> {
    let cfg = web::Data::new(cfg);
    let cfg_clone = cfg.clone();

    let server = HttpServer::new(move || {
        App::new()
            .app_data(cfg_clone.clone())
            .route("/", web::get().to(index))
            .route("/ws", web::get().to(ws))
            .route("/static/{filename:.*}", web::get().to(static_file))
    });

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
