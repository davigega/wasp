// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;
use crate::rooms::{Room, Rooms};
use crate::subscriber::Subscriber;

use std::sync::{Arc, Mutex};

use anyhow::Error;

use actix::{Actor, Addr, Handler, Message, StreamHandler};

use actix_web_actors::ws;

use log::debug;

/// Actor that represents a WebRTC publisher.
#[derive(Debug)]
pub struct Publisher {
    cfg: Arc<Config>,
    rooms: Addr<Rooms>,
    room: Mutex<Option<Addr<Room>>>,
}

impl Publisher {
    /// Create a new `Publisher` actor.
    pub fn new(cfg: Arc<Config>, rooms: Addr<Rooms>) -> Self {
        Publisher {
            cfg,
            rooms,
            room: Mutex::new(None),
        }
    }
}

impl Actor for Publisher {
    type Context = ws::WebsocketContext<Self>;
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Publisher {
    fn handle(&mut self, _msg: Result<ws::Message, ws::ProtocolError>, _ctx: &mut Self::Context) {
        // TODO
    }
}

/// New `Subscriber` joining the `Room` of this `Publisher`.
#[derive(Debug)]
pub struct NewSubscriberMessage {
    pub subscriber: Addr<Subscriber>,
}

impl Message for NewSubscriberMessage {
    type Result = Result<(), Error>;
}

impl Handler<NewSubscriberMessage> for Publisher {
    type Result = Result<(), Error>;

    fn handle(
        &mut self,
        msg: NewSubscriberMessage,
        _ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        debug!("New subscriber {:?} joining", msg.subscriber);

        // TODO
        Ok(())
    }
}

/// Existing `Subscriber` leaving the `Room` of this `Publisher`.
#[derive(Debug)]
pub struct LeavingSubscriberMessage {
    pub subscriber: Addr<Subscriber>,
}

impl Message for LeavingSubscriberMessage {
    type Result = Result<(), Error>;
}

impl Handler<LeavingSubscriberMessage> for Publisher {
    type Result = Result<(), Error>;

    fn handle(
        &mut self,
        msg: LeavingSubscriberMessage,
        _ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        debug!("Subscriber {:?} leaving", msg.subscriber);

        // TODO
        Ok(())
    }
}
