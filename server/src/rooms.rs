// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use actix::{Actor, ActorContext, Addr, AsyncContext, Context, Handler, Message, MessageResult};
use anyhow::{bail, Error};
use log::{debug, error, info, trace};

use crate::publisher::{self, Publisher};
use crate::subscriber::{self, Subscriber};

/// Unique identifier for a `Room`.
#[derive(Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub struct RoomId(u64);

/// Message to create a new `Room` for a `Publisher`.
#[derive(Debug)]
pub struct CreateRoomMessage {
    pub publisher: Addr<Publisher>,
    pub name: String,
    pub description: Option<String>,
}

impl Message for CreateRoomMessage {
    type Result = Result<Addr<Room>, Error>;
}

/// Message to find an existing `Room`.
#[derive(Debug)]
pub struct FindRoomMessage {
    pub room_id: RoomId,
}

impl Message for FindRoomMessage {
    type Result = Option<Addr<Room>>;
}

/// Message to list all currently existing `Room`s.
#[derive(Debug)]
pub struct ListRoomsMessage;

impl Message for ListRoomsMessage {
    type Result = Vec<Addr<Room>>;
}

/// Sent from a `Room` to remove itself from the list of `Rooms` after the `Publisher` deleted it.
#[derive(Debug)]
struct RoomDeletedMessage {
    room_id: RoomId,
}

impl Message for RoomDeletedMessage {
    type Result = ();
}

/// Actor that keeps track of all currently existing `Room`s.
#[derive(Debug)]
pub struct Rooms {
    rooms: Mutex<HashMap<RoomId, Addr<Room>>>,
}

impl Rooms {
    /// Create a new `Rooms` instance.
    pub fn new() -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
        }
    }
}

impl Actor for Rooms {
    type Context = Context<Self>;
}

impl Handler<CreateRoomMessage> for Rooms {
    type Result = Result<Addr<Room>, Error>;

    fn handle(&mut self, msg: CreateRoomMessage, ctx: &mut Context<Self>) -> Self::Result {
        info!("Creating new room for message {:?}", msg);

        let mut rooms = self.rooms.lock().unwrap();

        let room = Room::new(ctx.address(), msg.name, msg.description, msg.publisher);
        let room_id = room.id;

        info!("Created new room {:?}", room_id);

        let room_addr = room.start();
        rooms.insert(room_id, room_addr.clone());

        Ok(room_addr)
    }
}

impl Handler<FindRoomMessage> for Rooms {
    type Result = Option<Addr<Room>>;

    fn handle(&mut self, msg: FindRoomMessage, _ctx: &mut Context<Self>) -> Self::Result {
        debug!("Finding room {:?}", msg.room_id);

        let rooms = self.rooms.lock().unwrap();
        rooms.get(&msg.room_id).cloned()
    }
}

impl Handler<ListRoomsMessage> for Rooms {
    type Result = MessageResult<ListRoomsMessage>;

    fn handle(&mut self, _msg: ListRoomsMessage, _ctx: &mut Context<Self>) -> Self::Result {
        debug!("Listing all current rooms");

        MessageResult(self.rooms.lock().unwrap().values().cloned().collect())
    }
}

impl Handler<RoomDeletedMessage> for Rooms {
    type Result = ();

    fn handle(&mut self, msg: RoomDeletedMessage, _ctx: &mut Context<Self>) -> Self::Result {
        let mut rooms = self.rooms.lock().unwrap();

        info!("Room {:?} destroyed", msg.room_id);

        rooms.remove(&msg.room_id).expect("Room not found");
    }
}

/// Request `RoomInformation` from a given `Room`.
#[derive(Debug)]
pub struct RoomInformationMessage;

/// Room information returned from the `RoomInformationMessage`.
#[derive(Debug)]
pub struct RoomInformation {
    pub id: RoomId,
    pub name: String,
    pub description: Option<String>,
}

impl Message for RoomInformationMessage {
    type Result = RoomInformation;
}

/// Delete a `Room` from a `Publisher`.
#[derive(Debug)]
pub struct DeleteRoomMessage {
    pub publisher: Addr<Publisher>,
}

impl Message for DeleteRoomMessage {
    type Result = Result<(), Error>;
}

/// Join a `Room` by a `Subscriber`.
#[derive(Debug)]
pub struct JoinRoomMessage {
    pub subscriber: Addr<Subscriber>,
}

impl Message for JoinRoomMessage {
    type Result = Result<(), Error>;
}

/// Leave a `Room` by a `Subscriber`.
#[derive(Debug)]
pub struct LeaveRoomMessage {
    pub subscriber: Addr<Subscriber>,
}

impl Message for LeaveRoomMessage {
    type Result = Result<(), Error>;
}

/// Actor that manages a `Room` with its `Publisher` and `Subscriber`s.
#[derive(Debug)]
pub struct Room {
    id: RoomId,
    name: String,
    description: Option<String>,

    rooms: Addr<Rooms>,

    publisher: Addr<Publisher>,
    subscribers: Mutex<HashSet<Addr<Subscriber>>>,
}

impl Room {
    /// Returns a next valid `RoomId`.
    fn next_room_id() -> RoomId {
        // XXX: We assume that no more than 2**64 rooms are ever created
        use std::sync::atomic::{self, AtomicU64};

        static ROOM_ID: AtomicU64 = AtomicU64::new(0);

        RoomId(ROOM_ID.fetch_add(1, atomic::Ordering::Relaxed))
    }

    /// Create a new `Room`.
    fn new(
        rooms: Addr<Rooms>,
        name: String,
        description: Option<String>,
        publisher: Addr<Publisher>,
    ) -> Self {
        Room {
            rooms,
            id: Self::next_room_id(),
            name,
            description,
            publisher,
            subscribers: Mutex::new(HashSet::new()),
        }
    }
}

impl Actor for Room {
    type Context = Context<Self>;

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        trace!("Room {:?} stopped", self.id);
    }
}

impl Handler<DeleteRoomMessage> for Room {
    type Result = Result<(), Error>;

    fn handle(&mut self, msg: DeleteRoomMessage, ctx: &mut Context<Self>) -> Self::Result {
        if self.publisher != msg.publisher {
            error!(
                "Tried to delete room {:?} from wrong publisher {:?}",
                self.id, msg.publisher
            );
            bail!("Deleting room {:?} not permitted", self.id);
        }

        info!("Deleting room {:?}", self.id);

        {
            let mut subscribers = self.subscribers.lock().unwrap();
            for subscriber in subscribers.drain() {
                subscriber.do_send(subscriber::RoomDeletedMessage);
            }
        }
        self.rooms.do_send(RoomDeletedMessage { room_id: self.id });

        ctx.stop();

        Ok(())
    }
}

impl Handler<JoinRoomMessage> for Room {
    type Result = Result<(), Error>;

    fn handle(&mut self, msg: JoinRoomMessage, _ctx: &mut Context<Self>) -> Self::Result {
        info!(
            "Joining room {:?} by subscriber {:?}",
            self.id, msg.subscriber
        );

        self.publisher.do_send(publisher::NewSubscriberMessage {
            subscriber: msg.subscriber.clone(),
        });

        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.insert(msg.subscriber);

        Ok(())
    }
}

impl Handler<LeaveRoomMessage> for Room {
    type Result = Result<(), Error>;

    fn handle(&mut self, msg: LeaveRoomMessage, _ctx: &mut Context<Self>) -> Self::Result {
        info!(
            "Leaving room {:?} by subscriber {:?}",
            self.id, msg.subscriber
        );

        {
            let mut subscribers = self.subscribers.lock().unwrap();
            if !subscribers.remove(&msg.subscriber) {
                error!(
                    "Room {:?} didn't have subscriber {:?}",
                    self.id, msg.subscriber
                );
                bail!(
                    "Room {:?} didn't have subscriber {:?}",
                    self.id,
                    msg.subscriber
                );
            }
        }

        self.publisher.do_send(publisher::LeavingSubscriberMessage {
            subscriber: msg.subscriber.clone(),
        });

        Ok(())
    }
}

impl Handler<RoomInformationMessage> for Room {
    type Result = MessageResult<RoomInformationMessage>;

    fn handle(&mut self, _msg: RoomInformationMessage, _ctx: &mut Context<Self>) -> Self::Result {
        debug!("Returning room information for room {:?}", self.id);

        MessageResult(RoomInformation {
            id: self.id,
            name: self.name.clone(),
            description: self.description.clone(),
        })
    }
}
