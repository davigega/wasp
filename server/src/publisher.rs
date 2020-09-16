// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;
use crate::rooms::{Room, Rooms};
use crate::subscriber::Subscriber;

use anyhow::{format_err, Error};

use std::sync::{Arc, Mutex};

use actix::{Actor, Addr, Handler, Message, StreamHandler, WeakAddr};
use actix_web::dev::ConnectionInfo;
use actix_web_actors::ws;

use log::{debug, trace};

/// Actor that represents a WebRTC publisher.
#[derive(Debug)]
pub struct Publisher {
    cfg: Arc<Config>,
    rooms: WeakAddr<Rooms>,
    remote_addr: String,
    room: Mutex<Option<Addr<Room>>>,
}

impl Publisher {
    /// Create a new `Publisher` actor.
    pub fn new(
        cfg: Arc<Config>,
        rooms: Addr<Rooms>,
        connection_info: &ConnectionInfo,
    ) -> Result<Self, Error> {
        debug!("Creating new publisher {:?}", connection_info);

        let remote_addr = connection_info
            .realip_remote_addr()
            .ok_or_else(|| format_err!("WebSocket connection without remote address"))?;

        Ok(Publisher {
            cfg,
            rooms: rooms.downgrade(),
            remote_addr: String::from(remote_addr),
            room: Mutex::new(None),
        })
    }
}

impl Actor for Publisher {
    type Context = ws::WebsocketContext<Self>;

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        // Drop reference to the joined room, if any
        self.room.lock().unwrap().take();

        trace!("Publisher {} stopped", self.remote_addr);
    }
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
