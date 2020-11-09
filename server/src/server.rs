// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::api;
use crate::config::Config;
use crate::publisher::Publisher;
use crate::rooms::Rooms;
use crate::subscriber::Subscriber;

use actix::{Actor, Addr};
use actix_files::NamedFile;
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;

use log::error;

/// Serve `index.html` for path `/`.
async fn index(cfg: web::Data<Config>) -> Result<NamedFile, actix_web::Error> {
    let full_path = cfg.static_files.join("index.html");

    let file = NamedFile::open(full_path)?;

    Ok(file.use_last_modified(true))
}

/// Create Subscriber/Publisher WebSocket actors.
async fn ws(
    cfg: web::Data<Config>,
    rooms: web::Data<Addr<Rooms>>,
    path: web::Path<String>,
    req: HttpRequest,
    stream: web::Payload,
) -> Result<HttpResponse, actix_web::Error> {
    match path.as_str() {
        "publish" => {
            let publisher = Publisher::new(
                cfg.into_inner(),
                rooms.as_ref().clone(),
                &req.connection_info(),
            )
            .map_err(|err| {
                error!("Failed to create publisher: {}", err);
                HttpResponse::InternalServerError()
            })?;

            ws::start(publisher, &req, stream)
        }
        "subscribe" => {
            let subscriber = Subscriber::new(
                cfg.into_inner(),
                rooms.as_ref().clone(),
                &req.connection_info(),
            )
            .map_err(|err| {
                error!("Failed to create subscriber: {}", err);
                HttpResponse::InternalServerError()
            })?;

            ws::start(subscriber, &req, stream)
        }
        _ => Ok(HttpResponse::NotFound().finish()),
    }
}

/// Serve static files (`index.html`, JavaScript, CSS, etc).
async fn static_file(
    cfg: web::Data<Config>,
    req: HttpRequest,
) -> Result<NamedFile, actix_web::Error> {
    let path: std::path::PathBuf = req.match_info().query("filename").parse().unwrap();
    let full_path = cfg.static_files.join(path);

    let file = NamedFile::open(full_path)?;

    Ok(file.use_last_modified(true))
}

/// Start the server based on the passed `Config`.
pub async fn run(cfg: Config) -> Result<(), anyhow::Error> {
    let rooms = Rooms::new().start();
    let rooms = web::Data::new(rooms);

    let cfg = web::Data::new(cfg);
    let cfg_clone = cfg.clone();

    let server = HttpServer::new(move || {
        let cors = actix_cors::Cors::default().allow_any_origin().max_age(3600);

        App::new()
            .wrap(actix_web::middleware::Logger::default())
            .wrap(cors)
            .app_data(cfg_clone.clone())
            .app_data(rooms.clone())
            .route("/", web::get().to(index))
            .route("/ws/{mode:(publish|subscribe)}", web::get().to(ws))
            .route("/static/{filename:.*}", web::get().to(static_file))
            .route("/api/rooms", web::get().to(api::rooms))
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
