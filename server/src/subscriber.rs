// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;
use crate::rooms::{Room, Rooms};

use anyhow::{format_err, Error};

use std::sync::{Arc, Mutex};

use actix::{Actor, Addr, Handler, Message, StreamHandler, WeakAddr};
use actix_web::dev::ConnectionInfo;
use actix_web_actors::ws;

use log::{debug, trace};

/// Actor that represents a WebRTC subscriber.
#[derive(Debug)]
pub struct Subscriber {
    cfg: Arc<Config>,
    rooms: WeakAddr<Rooms>,
    remote_addr: String,
    room: Mutex<Option<WeakAddr<Room>>>,
}

impl Subscriber {
    /// Create a new `Subscriber` actor.
    pub fn new(
        cfg: Arc<Config>,
        rooms: Addr<Rooms>,
        connection_info: &ConnectionInfo,
    ) -> Result<Self, Error> {
        debug!("Creating new subscriber {:?}", connection_info);

        let remote_addr = connection_info
            .realip_remote_addr()
            .ok_or_else(|| format_err!("WebSocket connection without remote address"))?;

        Ok(Subscriber {
            cfg,
            rooms: rooms.downgrade(),
            remote_addr: String::from(remote_addr),
            room: Mutex::new(None),
        })
    }
}

impl Actor for Subscriber {
    type Context = ws::WebsocketContext<Self>;

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        // Drop reference to the joined room, if any
        self.room.lock().unwrap().take();

        trace!("Subscriber {} stopped", self.remote_addr);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Subscriber {
    fn handle(&mut self, _msg: Result<ws::Message, ws::ProtocolError>, _ctx: &mut Self::Context) {
        // TODO
    }
}

/// Leave a `Room` by a `Subscriber`.
#[derive(Debug)]
pub struct RoomDeletedMessage;

impl Message for RoomDeletedMessage {
    type Result = ();
}

impl Handler<RoomDeletedMessage> for Subscriber {
    type Result = ();

    fn handle(
        &mut self,
        _msg: RoomDeletedMessage,
        _ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        debug!("Room deleted");

        // Drop reference to the joined room
        self.room.lock().unwrap().take();

        // TODO
    }
}
