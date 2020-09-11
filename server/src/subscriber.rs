// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;
use crate::rooms::{Room, Rooms};

use std::sync::{Arc, Mutex};

use actix::{Actor, Addr, Handler, Message, StreamHandler};

use actix_web_actors::ws;

use log::debug;

/// Actor that represents a WebRTC subscriber.
#[derive(Debug)]
pub struct Subscriber {
    cfg: Arc<Config>,
    rooms: Addr<Rooms>,
    room: Mutex<Option<Addr<Room>>>,
}

impl Subscriber {
    /// Create a new `Subscriber` actor.
    pub fn new(cfg: Arc<Config>, rooms: Addr<Rooms>) -> Self {
        Subscriber {
            cfg,
            rooms,
            room: Mutex::new(None),
        }
    }
}

impl Actor for Subscriber {
    type Context = ws::WebsocketContext<Self>;
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

        // TODO
    }
}
