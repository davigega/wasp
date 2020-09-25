// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::config::Config;
use crate::rooms::{Room, RoomId, Rooms};
use crate::subscriber::Subscriber;

use std::collections::HashMap;
use std::mem;
use std::sync::{Arc, Mutex};

use anyhow::{bail, format_err, Error};

use futures::prelude::*;

use actix::{
    prelude::*, Actor, ActorContext, Addr, AsyncContext, Handler, Message, StreamHandler, WeakAddr,
};
use actix_web::dev::ConnectionInfo;
use actix_web_actors::ws;

use log::{debug, error, trace, warn};

use gst::prelude::*;
use gst_base::prelude::*;

use webrtc_audio_publishing::publisher::{PublisherMessage, ServerMessage};

/// Room state.
#[derive(Debug)]
enum RoomState {
    None,
    Joining,
    Joined(WeakAddr<Room>, RoomId),
}

/// Stores all current subscribes and state relevant for them.
#[derive(Debug)]
struct Subscribers {
    latency: gst::ClockTime,
    subscribers: HashMap<Addr<Subscriber>, gst_app::AppSrc>,
}

/// Actor that represents a WebRTC publisher.
///
/// This manages a GStreamer pipeline with a webrtcbin connected to an appsink. The appsink is
/// forwarding all received RTP packets to the appsrcs of all subscribers.
#[derive(Debug)]
pub struct Publisher {
    cfg: Arc<Config>,
    rooms: WeakAddr<Rooms>,
    remote_addr: String,
    room_state: RoomState,

    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    app_sink: gst_app::AppSink,

    subscribers: Arc<Mutex<Subscribers>>,
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

        let pipeline = gst::Pipeline::new(Some(&format!("publisher-{}", remote_addr)));
        let webrtcbin = gst::ElementFactory::make(
            "webrtcbin",
            Some(&format!("webrtcbin-publisher-{}", remote_addr)),
        )
        .expect("Failed to create webrtcbin");
        let app_sink = gst::ElementFactory::make("appsink", None)
            .expect("Failed to create appsink")
            .downcast::<gst_app::AppSink>()
            .expect("Failed to downcast to appsink");

        app_sink.set_caps(Some(&gst::Caps::builder("application/x-rtp").build()));
        // Synchronization happens in the subscriber pipelines at the very end, no need to
        // delay buffers here in addition.
        app_sink.set_sync(false);

        // Use the same clock and base time in publisher and subscriber pipelines
        // for synchronization purposes.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        pipeline.set_start_time(gst::CLOCK_TIME_NONE);
        pipeline.set_base_time(gst::ClockTime::from(0));

        pipeline.add(&webrtcbin).unwrap();
        pipeline.add(&app_sink).unwrap();

        if let Some(ref stun_server) = cfg.stun_server {
            webrtcbin.set_property_from_str("stun-server", stun_server);
        }
        if let Some(ref turn_server) = cfg.turn_server {
            webrtcbin.set_property_from_str("turn-server", turn_server);
        }
        webrtcbin.set_property_from_str("bundle-policy", "max-bundle");

        let subscribers = Arc::new(Mutex::new(Subscribers {
            latency: gst::CLOCK_TIME_NONE,
            subscribers: HashMap::new(),
        }));

        Ok(Publisher {
            cfg,
            rooms: rooms.downgrade(),
            remote_addr: String::from(remote_addr),
            room_state: RoomState::None,

            pipeline,
            webrtcbin,
            app_sink,

            subscribers,
        })
    }

    /// Creates a future for creating the room asynchronously, storing its information
    /// and notifying the peer.
    fn create_room_future(
        &self,
        addr: Addr<Self>,
        rooms_addr: Addr<crate::rooms::Rooms>,
        name: String,
        description: Option<String>,
    ) -> impl ActorFuture<Actor = Self, Output = ()> {
        rooms_addr
            .send(crate::rooms::CreateRoomMessage {
                publisher: addr,
                name,
                description,
            })
            .into_actor(self)
            .then(move |res, s, ctx| {
                // Check if room creation was successful
                match res {
                    Ok(Ok((room, room_id))) => {
                        debug!("Created room {:?} with id {}", room, room_id);

                        match s.room_state {
                            RoomState::Joining => {
                                s.room_state = RoomState::Joined(room.downgrade(), room_id);

                                ctx.text(
                                    serde_json::to_string(&ServerMessage::RoomCreated {
                                        id: room_id.0,
                                    })
                                    .expect("Failed to serialize room created message"),
                                );
                            }
                            RoomState::Joined(_, room_id) => {
                                // This can't really happen, can it?
                                error!(
                                    "Already joined room {}",
                                    room_id
                                );
                            }
                            RoomState::None => {
                                debug!("Publisher {} not currently joining a room, deleting room again", s.remote_addr);

                                room.do_send(crate::rooms::DeleteRoomMessage { publisher: ctx.address() });
                            }
                        }
                    }
                    Ok(Err(err)) => {
                        error!("Failed to create room {:?}", err);
                        s.room_state = RoomState::None;
                        ctx.notify(ErrorMessage(String::from("Failed to create room")));
                    }
                    Err(err) => {
                        s.room_state = RoomState::None;
                        error!("Failed to create room {:?}", err);
                        ctx.notify(ErrorMessage(String::from("Failed to create room")));
                    }
                }

                actix::fut::ready(())
            })
    }

    /// Creates a future for creating the room asynchronously, storing its information
    /// and notifying the peer.
    fn handle_sdp_future(
        &self,
        sdp_message: gst_sdp::SDPMessage,
    ) -> impl ActorFuture<Actor = Self, Output = ()> {
        let pipeline = self.pipeline.clone();
        let webrtcbin = self.webrtcbin.clone();
        async move {
            let res = pipeline
                .call_async_future(|pipeline| {
                    debug!("Starting pipeline {}", pipeline.get_name());
                    pipeline.set_state(gst::State::Playing)
                })
                .await;
            if res.is_err() {
                error!("Failed to set pipeline to Playing");
                return Err(String::from("Failed to set pipeline to Playing"));
            }

            debug!("Setting remote description {:#?}", sdp_message);
            let offer = gst_webrtc::WebRTCSessionDescription::new(
                gst_webrtc::WebRTCSDPType::Offer,
                sdp_message,
            );

            webrtcbin
                .emit("set-remote-description", &[&offer, &None::<gst::Promise>])
                .expect("Failed to emit set-remote-offer signal");

            debug!("Creating answer for offer");
            let (promise, fut) = gst::Promise::new_future();
            webrtcbin
                .emit("create-answer", &[&None::<gst::Structure>, &promise])
                .unwrap();

            let res = match fut.await {
                Ok(Some(res)) => res,
                Ok(None) => {
                    error!("Empty answer");
                    return Err(String::from("Failed to create answer SDP"));
                }
                Err(err) => {
                    error!("Failed to create answer SDP: {:?}", err);
                    return Err(String::from("Failed to create answer SDP"));
                }
            };

            let answer = res
                .get_value("answer")
                .expect("Promise response didn't contain answer")
                .get::<gst_webrtc::WebRTCSessionDescription>()
                .expect("Invalid argument")
                .expect("Empty answer description");
            webrtcbin
                .emit("set-local-description", &[&answer, &None::<gst::Promise>])
                .expect("Failed to emit set-local-description signal");

            debug!("Created answer description {:#?}", answer.get_sdp());
            Ok(answer)
        }
        .into_actor(self)
        .then(|res, _s, ctx| {
            match res {
                Ok(answer) => {
                    ctx.text(
                        serde_json::to_string(&ServerMessage::Sdp {
                            type_: String::from("answer"),
                            sdp: answer
                                .get_sdp()
                                .as_text()
                                .expect("Can't convert answer SDP to text"),
                        })
                        .expect("Failed to serialize error message"),
                    );
                }
                Err(err) => {
                    ctx.notify(ErrorMessage(err));
                }
            }
            actix::fut::ready(())
        })
    }

    /// Handle JSON messages from the publisher.
    fn handle_message(&mut self, ctx: &mut ws::WebsocketContext<Self>, text: &str) {
        match serde_json::from_str::<PublisherMessage>(text) {
            Ok(PublisherMessage::CreateRoom {
                name,
                description,
                latency,
            }) => {
                debug!(
                    "Publisher {} asked to create new room {} with description {:?} and latency {:?}",
                    self.remote_addr, name, description, latency,
                );

                self.webrtcbin
                    .set_property("latency", &latency.unwrap_or(200))
                    .expect("Failed to set latency property");

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
                    ctx.spawn(self.create_room_future(ctx.address(), rooms, name, description));
                }
            }
            Ok(PublisherMessage::Ice {
                candidate,
                sdp_mline_index,
            }) => {
                debug!(
                    "Publisher {} sent ICE candidate {} at mline index {}",
                    self.remote_addr, candidate, sdp_mline_index
                );
                self.webrtcbin
                    .emit("add-ice-candidate", &[&sdp_mline_index, &candidate])
                    .unwrap();
            }
            Ok(PublisherMessage::Sdp { type_, sdp }) => {
                debug!("Publisher {} sent SDP", self.remote_addr);

                if type_ != "offer" {
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
                            error!("No room created yet");
                            ctx.notify(ErrorMessage(String::from("No room created yet")));
                            return;
                        }
                        _ => (),
                    }
                }

                // Asynchronously handle the SDP by starting the pipeline and creating the answer SDP.
                ctx.spawn(self.handle_sdp_future(sdp_message));
            }
            Err(err) => {
                error!(
                    "Publisher {} has websocket error: {}",
                    self.remote_addr, err
                );
                ctx.notify(ErrorMessage(String::from("Internal processing error")));
                self.shutdown(ctx, false);
            }
        }
    }

    /// Shut down this publisher and delete its room.
    fn shutdown(&mut self, ctx: &mut ws::WebsocketContext<Self>, from_close: bool) {
        debug!("Shutting down publisher {}", self.remote_addr);

        let _ = self.pipeline.set_state(gst::State::Null);

        if let RoomState::Joined(ref room, _) = mem::replace(&mut self.room_state, RoomState::None)
        {
            if let Some(room) = room.upgrade() {
                room.do_send(crate::rooms::DeleteRoomMessage {
                    publisher: ctx.address(),
                });
            }
        }

        {
            let mut subscribers = self.subscribers.lock().unwrap();
            subscribers.subscribers.clear();
        }

        self.app_sink
            .set_callbacks(gst_app::AppSinkCallbacks::builder().build());

        if !from_close {
            ctx.close(None);
        }
        ctx.stop();
    }

    /// Called whenever a new incoming stream is provided by webrtcbin.
    fn handle_new_incoming_stream(
        addr: &Addr<Self>,
        webrtcbin: &gst::Element,
        pad: &gst::Pad,
        app_sink: &gst_app::AppSink,
    ) {
        let caps = pad.get_current_caps();
        debug!(
            "Got incoming stream on webrtcbin {} with caps {:?}",
            webrtcbin.get_name(),
            caps
        );

        let sinkpad = app_sink
            .get_static_pad("sink")
            .expect("appsink has no sinkpad");
        if let Some(peer) = sinkpad.get_peer() {
            trace!("appsink already connected to {}", peer.get_path_string());
            let _ = peer.unlink(&sinkpad);
        }

        if let Err(err) = pad.link(&sinkpad) {
            error!(
                "Failed to link webrtcbin {} to appsink: {:?}",
                webrtcbin.get_name(),
                err
            );
            addr.do_send(ErrorMessage(String::from(
                "Failed to handle incoming stream",
            )));
        }
    }

    /// Called whenever the appsink has a new sample. Forward sample to all current subscribers.
    fn handle_new_sample(
        addr: Addr<Self>,
        app_sink: &gst_app::AppSink,
        subscribers: &Arc<Mutex<Subscribers>>,
    ) {
        let subscribers = subscribers
            .lock()
            .unwrap()
            .subscribers
            .iter()
            .map(|(a, s)| (a.clone(), s.clone()))
            .collect::<Vec<_>>();

        let sample = match app_sink.pull_sample() {
            Ok(sample) => sample,
            Err(_err) => {
                // Can only really happen if we're just shutting down now
                return;
            }
        };

        trace!("Sending sample {:?} from publisher {:?}", sample, addr);
        for (sub_addr, subscriber) in subscribers {
            let current_level_bytes = subscriber.get_current_level_bytes();
            let max_bytes = subscriber.get_max_bytes();

            if current_level_bytes >= max_bytes && max_bytes != 0 {
                warn!("Not sending from publisher {:?} to subscriber {:?} because of buffering limits: {} >= {}", addr, sub_addr, current_level_bytes, max_bytes);
            } else if let Err(err) = subscriber.push_sample(&sample) {
                warn!(
                    "Error sending from publisher {:?} to subscriber {:?}: {:?}",
                    addr, sub_addr, err
                );
            }
        }
    }

    /// Called whenever the appsink received EOS. Forward the EOS to all the current subscribers.
    fn handle_eos(
        addr: Addr<Self>,
        _app_sink: &gst_app::AppSink,
        subscribers: &Arc<Mutex<Subscribers>>,
    ) {
        let subscribers = subscribers
            .lock()
            .unwrap()
            .subscribers
            .iter()
            .map(|(a, s)| (a.clone(), s.clone()))
            .collect::<Vec<_>>();
        for (sub_addr, subscriber) in subscribers {
            if let Err(err) = subscriber.end_of_stream() {
                error!(
                    "Sending EOS from publisher {:?} to subscriber {:?} failed: {:?}",
                    addr, sub_addr, err
                );
            }
        }
    }
}

impl Actor for Publisher {
    type Context = ws::WebsocketContext<Self>;

    /// Called once the publisher is started to set up the pipeline.
    fn started(&mut self, ctx: &mut Self::Context) {
        trace!("Started publisher {}", self.remote_addr);

        let addr = ctx.address().downgrade();
        let app_sink_clone = self.app_sink.clone();
        self.webrtcbin.connect_pad_added(move |webrtcbin, pad| {
            if let Some(addr) = addr.upgrade() {
                Self::handle_new_incoming_stream(&addr, webrtcbin, pad, &app_sink_clone);
            }
        });

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

        let subscribers_clone = self.subscribers.clone();
        let subscribers_clone_2 = self.subscribers.clone();
        let addr = ctx.address().downgrade();
        let addr_2 = ctx.address().downgrade();
        self.app_sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |app_sink| {
                    if let Some(addr) = addr.upgrade() {
                        Self::handle_new_sample(addr, app_sink, &subscribers_clone);

                        Ok(gst::FlowSuccess::Ok)
                    } else {
                        Err(gst::FlowError::Eos)
                    }
                })
                .eos(move |app_sink| {
                    if let Some(addr) = addr_2.upgrade() {
                        Self::handle_eos(addr, app_sink, &subscribers_clone_2);
                    }
                })
                .build(),
        );

        // Catch Latency events and update all subscribers whenever it changes.
        let subscribers_clone = self.subscribers.clone();
        let pad = self
            .app_sink
            .get_static_pad("sink")
            .expect("No sink pad on appsink");
        pad.add_probe(
            gst::PadProbeType::EVENT_UPSTREAM,
            move |_pad, info| match info.data {
                Some(gst::PadProbeData::Event(ref ev))
                    if ev.get_type() == gst::EventType::Latency =>
                {
                    let latency = match ev.view() {
                        gst::EventView::Latency(ev) => ev.get_latency(),
                        _ => unreachable!(),
                    };

                    trace!("Got new latency {}, notifying subscribers", latency,);

                    let mut subscribers = subscribers_clone.lock().unwrap();
                    subscribers.latency = latency;
                    for subscriber in subscribers.subscribers.values() {
                        subscriber.set_latency(latency, gst::CLOCK_TIME_NONE);
                    }

                    gst::PadProbeReturn::Ok
                }
                _ => gst::PadProbeReturn::Ok,
            },
        );

        // Handle pipeline bus messages asynchronously.
        let bus = self.pipeline.get_bus().expect("Pipeline with no bus");
        let bus_stream = bus.stream();
        Self::add_stream(bus_stream.map(BusMessage), ctx);
    }

    /// Called when the publisher is fully stopped.
    fn stopped(&mut self, ctx: &mut Self::Context) {
        trace!("Publisher {} stopped", self.remote_addr);
        self.shutdown(ctx, false);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for Publisher {
    /// Handle websocket messages from the peer.
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
                self.shutdown(ctx, true);
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
                    "Publisher {} websocket connection error: {:?}",
                    self.remote_addr, err
                );
                self.shutdown(ctx, false);
            }
        }
    }
}

/// New `Subscriber` joining the `Room` of this `Publisher`.
#[derive(Debug)]
pub struct NewSubscriberMessage {
    pub subscriber: Addr<Subscriber>,
    pub app_src: gst_app::AppSrc,
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

        let mut subscribers = self.subscribers.lock().unwrap();
        if subscribers.latency.is_some() {
            msg.app_src
                .set_latency(subscribers.latency, gst::CLOCK_TIME_NONE);
        }
        subscribers.subscribers.insert(msg.subscriber, msg.app_src);

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

        if self
            .subscribers
            .lock()
            .unwrap()
            .subscribers
            .remove(&msg.subscriber)
            .is_none()
        {
            error!("Not subscribed to publisher {}", self.remote_addr);
            bail!("Not subscribed to publisher");
        }

        Ok(())
    }
}

/// Message to report pipeline errors to the actor.
#[derive(Debug)]
struct ErrorMessage(String);

impl Message for ErrorMessage {
    type Result = ();
}

impl Handler<ErrorMessage> for Publisher {
    type Result = ();

    fn handle(&mut self, msg: ErrorMessage, ctx: &mut ws::WebsocketContext<Self>) -> Self::Result {
        error!(
            "Got error message '{}' on publisher {}",
            msg.0, self.remote_addr
        );

        ctx.text(
            serde_json::to_string(&ServerMessage::Error { message: msg.0 })
                .expect("Failed to serialize error message"),
        );
        self.shutdown(ctx, false);
    }
}

/// SDP answer created by GStreamer to be sent back to the peer.
#[derive(Debug)]
struct SDPAnswerMessage(String);

impl Message for SDPAnswerMessage {
    type Result = ();
}

impl Handler<SDPAnswerMessage> for Publisher {
    type Result = ();

    fn handle(
        &mut self,
        msg: SDPAnswerMessage,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> Self::Result {
        trace!("Got answer SDP from publisher {}", self.remote_addr);

        ctx.text(
            serde_json::to_string(&ServerMessage::Sdp {
                type_: String::from("answer"),
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

impl Handler<ICECandidateMessage> for Publisher {
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

#[derive(Debug)]
struct BusMessage(gst::Message);

impl Message for BusMessage {
    type Result = ();
}

impl StreamHandler<BusMessage> for Publisher {
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
                debug!("Publisher {} updated latency", self.remote_addr);
                self.pipeline.call_async(|pipeline| {
                    let _ = pipeline.recalculate_latency();
                });
            }
            _ => (),
        }
    }

    fn finished(&mut self, _ctx: &mut Self::Context) {
        debug!("Finished bus messages for publisher {}", self.remote_addr);
    }
}
