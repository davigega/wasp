// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;
use crate::rooms::{self, Room, RoomId, Rooms};

use anyhow::{bail, format_err, Error};

use std::mem;
use std::sync::Arc;

use actix::{
    Actor, ActorContext, ActorFuture, Addr, AsyncContext, Handler, Message, StreamHandler,
    WeakAddr, WrapFuture,
};
use actix_web::dev::ConnectionInfo;
use actix_web_actors::ws;

use futures::prelude::*;

use log::{debug, error, trace, warn};

use webrtc_audio_publishing::subscriber::{ServerMessage, SubscriberMessage};

use gst::prelude::*;

/// Room state.
#[derive(Debug)]
enum RoomState {
    None,
    Joining,
    Joined(WeakAddr<Room>, RoomId),
}

/// Actor that represents a WebRTC subscriber.
#[derive(Debug)]
pub struct Subscriber {
    cfg: Arc<Config>,
    rooms: WeakAddr<Rooms>,
    remote_addr: String,
    room_state: RoomState,

    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    app_src: gst_app::AppSrc,
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

        let pipeline = gst::Pipeline::new(Some(&format!("subscriber-{}", remote_addr)));
        let webrtcbin = gst::ElementFactory::make(
            "webrtcbin",
            Some(&format!("webrtcbin-subscriber-{}", remote_addr)),
        )
        .expect("Failed to create webrtcbin");
        let app_src = gst::ElementFactory::make("appsrc", None)
            .expect("Failed to create appsrc")
            .downcast::<gst_app::AppSrc>()
            .expect("Failed to downcast to appsink");

        app_src.set_property_format(gst::Format::Time);
        app_src.set_property_is_live(true);

        // Allow up to 200kbyte, that's about 5 seconds of audio at 128 kbit/s
        app_src.set_max_bytes(80_000);

        // Latency on the appsrc is set by the publisher before the first buffer
        // and whenever it changes

        // Use the same clock and base time in publisher and subscriber pipelines
        // for synchronization purposes.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        pipeline.set_start_time(gst::CLOCK_TIME_NONE);
        pipeline.set_base_time(gst::ClockTime::from(0));

        pipeline.add(&webrtcbin).unwrap();
        pipeline.add(&app_src).unwrap();

        if let Some(ref stun_server) = cfg.stun_server {
            webrtcbin.set_property_from_str("stun-server", stun_server);
        }
        if let Some(ref turn_server) = cfg.turn_server {
            webrtcbin.set_property_from_str("turn-server", turn_server);
        }
        webrtcbin.set_property_from_str("bundle-policy", "max-bundle");

        app_src
            .link_pads(Some("src"), &webrtcbin, Some("sink_%u"))
            .expect("Couldn't link appsrc to webrtcbin");

        Ok(Subscriber {
            cfg,
            rooms: rooms.downgrade(),
            remote_addr: String::from(remote_addr),
            room_state: RoomState::None,

            pipeline,
            webrtcbin,
            app_src,
        })
    }

    /// Creates a future for creating the room asynchronously, storing its information
    /// and notifying the peer.
    fn join_room_future(
        &self,
        addr: Addr<Self>,
        rooms_addr: Addr<crate::rooms::Rooms>,
        room_id: RoomId,
    ) -> impl ActorFuture<Actor = Self, Output = ()> {
        let app_src = self.app_src.clone();
        async move {
            let res = rooms_addr
                .send(crate::rooms::FindRoomMessage { room_id })
                .await;

            // FIXME: This is not very beautiful
            let res = match res {
                Ok(Some(room)) => room
                    .send(rooms::JoinRoomMessage {
                        subscriber: addr,
                        app_src,
                    })
                    .await
                    .map(|res| res.map(|_| Some(room))),
                Ok(None) => Ok(Ok(None)),
                Err(err) => Err(err),
            };

            res
        }
        .into_actor(self)
        .then(move |res, s, ctx| {
            match res {
                Ok(Ok(Some(room))) => {
                    debug!(
                        "Subscriber {} successfully joined room {}",
                        s.remote_addr, room_id
                    );

                    match &s.room_state {
                        RoomState::Joining => {
                            s.room_state = RoomState::Joined(room.downgrade(), room_id);

                            // Now start the pipeline to generate the SDP
                            let addr = ctx.address().downgrade();
                            s.pipeline.call_async(move |pipeline| {
                                if let Some(addr) = addr.upgrade() {
                                    debug!("Starting pipeline");
                                    if pipeline.set_state(gst::State::Playing).is_err() {
                                        error!("Error starting pipeline");
                                        addr.do_send(ErrorMessage(String::from(
                                            "Failed to set pipeline to Playing",
                                        )));
                                    }
                                }
                            });
                        }
                        RoomState::Joined(_, room_id) => {
                            // This can't really happen, can it?
                            error!("Already joined room {}", room_id);
                        }
                        RoomState::None => {
                            debug!(
                                "Subscriber {} not currently joining a room, leaving room again",
                                s.remote_addr
                            );

                            room.do_send(crate::rooms::LeaveRoomMessage {
                                subscriber: ctx.address(),
                            });
                        }
                    }
                }
                Ok(Ok(None)) => {
                    debug!("Room {} not found", room_id);
                    s.room_state = RoomState::None;
                    ctx.notify(ErrorMessage(String::from("Room not found")));
                }
                Ok(Err(err)) => {
                    error!(
                        "Subscriber {} failed to join room {}: {:?}",
                        s.remote_addr, room_id, err
                    );
                    s.room_state = RoomState::None;
                    ctx.notify(ErrorMessage(String::from("Failed to join room")));
                }
                Err(err) => {
                    error!(
                        "Subscriber {} failed to join room {}: {:?}",
                        s.remote_addr, room_id, err
                    );
                    s.room_state = RoomState::None;
                    ctx.notify(ErrorMessage(String::from("Failed to join room")));
                }
            }

            actix::fut::ready(())
        })
    }

    /// Creates a future for starting negotiation with the peer up to sending the offer
    /// to the peer.
    fn start_negotiation_future(&self) -> impl ActorFuture<Actor = Self, Output = ()> {
        let webrtcbin = self.webrtcbin.clone();

        async move {
            let (promise, fut) = gst::Promise::new_future();
            webrtcbin
                .emit("create-offer", &[&None::<gst::Structure>, &promise])
                .expect("Failed to emit create-offer signal");
            let reply = fut.await;

            // Chech if we got a valid offer
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    bail!("Offer creation got no reponse");
                }
                Err(err) => {
                    bail!("Offer creation got error reponse: {:?}", err);
                }
            };

            let offer = reply
                .get_value("offer")
                .expect("Invalid argument")
                .get::<gst_webrtc::WebRTCSessionDescription>()
                .expect("Invalid argument")
                .unwrap();

            debug!("Created offer {:#?}", offer.get_sdp());

            webrtcbin
                .emit("set-local-description", &[&offer, &None::<gst::Promise>])
                .expect("Failed to emit set-local-description signal");

            Ok(offer)
        }
        .into_actor(self)
        .then(|res, _s, ctx| {
            match res {
                Ok(offer) => {
                    ctx.text(
                        serde_json::to_string(&ServerMessage::Sdp {
                            type_: String::from("offer"),
                            sdp: offer.get_sdp().as_text().expect("Invalid offer"),
                        })
                        .expect("Failed to serialize SDP message"),
                    );
                }
                Err(_err) => {
                    ctx.notify(ErrorMessage(String::from("Failed to create offer")));
                }
            }

            actix::fut::ready(())
        })
    }

    /// Handle JSON messages from the subscriber.
    fn handle_message(&mut self, ctx: &mut ws::WebsocketContext<Self>, text: &str) {
        match serde_json::from_str::<SubscriberMessage>(text) {
            Ok(SubscriberMessage::JoinRoom { id }) => {
                debug!("Subscriber {} asked to join room {}", self.remote_addr, id,);

                // Check if we can join a room currently.
                {
                    match &self.room_state {
                        RoomState::Joining => {
                            ctx.notify(ErrorMessage(String::from("Already joining a room")));

                            return;
                        }
                        RoomState::Joined(_, room_id) => {
                            ctx.notify(ErrorMessage(format!("Already joined room {}", room_id)));

                            return;
                        }
                        _ => {
                            self.room_state = RoomState::Joining;
                        }
                    }
                }

                if let Some(rooms) = self.rooms.upgrade() {
                    // Now tell the Rooms instance to create a room for us with the given parameters
                    // and return its address/UUID. This all happens asynchronously.
                    ctx.spawn(self.join_room_future(ctx.address(), rooms, RoomId(id)));
                }
            }
            Ok(SubscriberMessage::Ice {
                candidate,
                sdp_mline_index,
            }) => {
                debug!(
                    "Subscriber {} sent ICE candidate {} at mline index {}",
                    self.remote_addr, candidate, sdp_mline_index
                );
                self.webrtcbin
                    .emit("add-ice-candidate", &[&sdp_mline_index, &candidate])
                    .unwrap();
            }
            Ok(SubscriberMessage::Sdp { type_, sdp }) => {
                debug!("Subscriber {} sent SDP", self.remote_addr);

                if type_ != "answer" {
                    error!("SDP of wrong type {}", type_);
                    ctx.notify(ErrorMessage(String::from("Invalid SDP type")));
                    return;
                }

                let sdp_message = match gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()) {
                    Ok(sdp_message) => sdp_message,
                    Err(_) => {
                        error!("Failed to parse SDP");
                        ctx.notify(ErrorMessage(String::from("Invalid SDP")));
                        return;
                    }
                };

                {
                    match &self.room_state {
                        RoomState::None | RoomState::Joining => {
                            error!("No room joined yet");
                            ctx.notify(ErrorMessage(String::from("No room joined yet")));
                            return;
                        }
                        _ => (),
                    }
                }

                debug!("Setting remote description {:#?}", sdp_message);
                let answer = gst_webrtc::WebRTCSessionDescription::new(
                    gst_webrtc::WebRTCSDPType::Answer,
                    sdp_message,
                );

                self.webrtcbin
                    .emit("set-remote-description", &[&answer, &None::<gst::Promise>])
                    .expect("Failed to emit set-remote-description signal");
            }
            Err(err) => {
                error!(
                    "Publisher {} has websocket error: {}",
                    self.remote_addr, err
                );
                ctx.notify(ErrorMessage(String::from("Internal processing error")));
                self.shutdown(ctx);
            }
        }
    }

    /// Shut down this subscriber and delete its room.
    fn shutdown(&mut self, ctx: &mut ws::WebsocketContext<Self>) {
        debug!("Shutting down subscriber {}", self.remote_addr);

        let _ = self.pipeline.set_state(gst::State::Null);

        if let RoomState::Joined(ref room, _) = mem::replace(&mut self.room_state, RoomState::None)
        {
            if let Some(room) = room.upgrade() {
                room.do_send(crate::rooms::LeaveRoomMessage {
                    subscriber: ctx.address(),
                });
            }
        }

        ctx.close(None);
        ctx.stop();
    }
}

impl Actor for Subscriber {
    type Context = ws::WebsocketContext<Self>;

    /// Called once the subscriber is started to set up the pipeline.
    fn started(&mut self, ctx: &mut Self::Context) {
        trace!("Started subscriber {}", self.remote_addr);

        let addr = ctx.address().downgrade();
        self.webrtcbin
            .connect("on-negotiation-needed", false, move |values| {
                let _webrtc = values[0]
                    .get::<gst::Element>()
                    .expect("Invalid argument")
                    .unwrap();

                if let Some(addr) = addr.upgrade() {
                    addr.do_send(NegotiationNeededMessage);
                }

                None
            })
            .unwrap();

        let addr = ctx.address().downgrade();
        self.webrtcbin
            .connect("on-ice-candidate", false, move |args| {
                let _webrtc = args[0].get::<gst::Element>().expect("Invalid argument");
                let sdp_mline_index = args[1].get_some::<u32>().expect("Invalid argument");
                let candidate = args[2].get::<String>().expect("Invalid argument").unwrap();

                if let Some(addr) = addr.upgrade() {
                    addr.do_send(ICECandidateMessage {
                        candidate,
                        sdp_mline_index,
                    });
                }

                None
            })
            .expect("Failed to connect to on-ice-candidate");

        // Handle pipeline bus messages asynchronously.
        let bus = self.pipeline.get_bus().expect("Pipeline with no bus");
        let bus_stream = bus.stream();
        Self::add_stream(bus_stream.map(BusMessage), ctx);
    }

    /// Called when the subscriber is fully stopped.
    fn stopped(&mut self, ctx: &mut Self::Context) {
        trace!("Subscriber {} stopped", self.remote_addr);
        self.shutdown(ctx);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Subscriber {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Text(text)) => {
                self.handle_message(ctx, &text);
            }
            Ok(ws::Message::Close(reason)) => {
                debug!(
                    "Publisher {} websocket connection closed: {:?}",
                    self.remote_addr, reason
                );
                self.shutdown(ctx);
            }
            Ok(ws::Message::Binary(_binary)) => {
                error!("Unsupported binary message, ignoring");
            }
            Ok(ws::Message::Continuation(_)) => {
                error!("Unsupported continuation message, ignoring");
            }
            Ok(ws::Message::Nop) | Ok(ws::Message::Pong(_)) => {
                // Do nothing
            }
            Err(err) => {
                error!(
                    "Subscriber {} websocket connection error: {:?}",
                    self.remote_addr, err
                );
                self.shutdown(ctx);
            }
        }
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
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        debug!("Room deleted for subscriber {}", self.remote_addr);
        self.shutdown(ctx);
    }
}

/// Message to report pipeline errors to the actor.
#[derive(Debug)]
struct ErrorMessage(String);

impl Message for ErrorMessage {
    type Result = ();
}

impl Handler<ErrorMessage> for Subscriber {
    type Result = ();

    fn handle(&mut self, msg: ErrorMessage, ctx: &mut ws::WebsocketContext<Self>) -> Self::Result {
        error!(
            "Got error message '{}' on subscriber {}",
            msg.0, self.remote_addr
        );

        ctx.text(
            serde_json::to_string(&ServerMessage::Error { message: msg.0 })
                .expect("Failed to serialize error message"),
        );
        self.shutdown(ctx);
    }
}

/// SDP offer created by GStreamer to be sent back to the peer.
#[derive(Debug)]
struct SDPOfferMessage(String);

impl Message for SDPOfferMessage {
    type Result = ();
}

impl Handler<SDPOfferMessage> for Subscriber {
    type Result = ();

    fn handle(
        &mut self,
        msg: SDPOfferMessage,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        trace!("Got offer SDP from subscriber {}", self.remote_addr);

        ctx.text(
            serde_json::to_string(&ServerMessage::Sdp {
                type_: String::from("offer"),
                sdp: msg.0,
            })
            .expect("Failed to serialize SDP message"),
        );
    }
}

/// ICE candidate from GStreamer to be sent back to the peer.
#[derive(Debug)]
struct ICECandidateMessage {
    candidate: String,
    sdp_mline_index: u32,
}

impl Message for ICECandidateMessage {
    type Result = ();
}

impl Handler<ICECandidateMessage> for Subscriber {
    type Result = ();

    fn handle(
        &mut self,
        msg: ICECandidateMessage,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        trace!(
            "Got answer ICE candidate from publisher {}",
            self.remote_addr
        );

        ctx.text(
            serde_json::to_string(&ServerMessage::Ice {
                candidate: msg.candidate,
                sdp_mline_index: msg.sdp_mline_index,
            })
            .expect("Failed to serialize ICE message"),
        );
    }
}

/// ICE candidate from GStreamer to be sent back to the peer.
#[derive(Debug)]
struct NegotiationNeededMessage;

impl Message for NegotiationNeededMessage {
    type Result = ();
}

impl Handler<NegotiationNeededMessage> for Subscriber {
    type Result = ();

    fn handle(
        &mut self,
        _msg: NegotiationNeededMessage,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        trace!("Negotiation needed for publisher {}", self.remote_addr);

        ctx.spawn(self.start_negotiation_future());
    }
}

#[derive(Debug)]
struct BusMessage(gst::Message);

impl Message for BusMessage {
    type Result = ();
}

impl StreamHandler<BusMessage> for Subscriber {
    fn handle(&mut self, msg: BusMessage, ctx: &mut ws::WebsocketContext<Self>) {
        use gst::MessageView;

        trace!("Got message {:?} for publisher {}", msg.0, self.remote_addr);

        match msg.0.view() {
            MessageView::Error(err) => {
                let src = err.get_src();
                let dbg = err.get_debug();
                let err = err.get_error();

                if let Some(dbg) = dbg {
                    error!(
                        "Got error from {}: {} ({})",
                        src.map(|src| src.get_path_string())
                            .as_deref()
                            .unwrap_or("UNKNOWN"),
                        err,
                        dbg
                    );
                } else {
                    error!(
                        "Got error from {}: {}",
                        src.map(|src| src.get_path_string())
                            .as_deref()
                            .unwrap_or("UNKNOWN"),
                        err
                    );
                }

                ctx.notify(ErrorMessage(String::from("Internal processing error")));
            }
            MessageView::Warning(warn) => {
                let src = warn.get_src();
                let dbg = warn.get_debug();
                let err = warn.get_error();

                if let Some(dbg) = dbg {
                    warn!(
                        "Got warning from {}: {} ({})",
                        src.map(|src| src.get_path_string())
                            .as_deref()
                            .unwrap_or("UNKNOWN"),
                        err,
                        dbg
                    );
                } else {
                    warn!(
                        "Got warning from {}: {}",
                        src.map(|src| src.get_path_string())
                            .as_deref()
                            .unwrap_or("UNKNOWN"),
                        err
                    );
                }
            }
            MessageView::Latency(_) => {
                debug!("Subscriber {} updated latency", self.remote_addr);
                self.pipeline.call_async(|pipeline| {
                    let _ = pipeline.recalculate_latency();
                });
            }
            _ => (),
        }
    }

    fn finished(&mut self, _ctx: &mut Self::Context) {
        debug!("Finished bus messages for subscriber {}", self.remote_addr);
    }
}
