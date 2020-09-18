// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use anyhow::{bail, format_err, Context, Error};
use std::sync::{atomic, Arc};

use log::{debug, error, info, trace, warn};

use async_tungstenite::tungstenite;
use tungstenite::Message as WsMessage;

use futures::channel::mpsc;
use futures::prelude::*;

use gst::prelude::*;

use webrtc_audio_publishing::publisher::{PublisherMessage, ServerMessage};

use crate::config::Config;

/// Publisher control handle.
#[derive(Debug, Clone)]
pub struct Publisher {
    event_sender: mpsc::UnboundedSender<PublisherEvent>,
    stopped: Arc<atomic::AtomicBool>,
}

/// Future that can be awaited on to wait for the publisher to stop or error out.
#[derive(Debug)]
pub struct PublisherJoinHandle {
    handle: tokio::task::JoinHandle<Result<(), Error>>,
}

/// Simply wrapping around the tokio `JoinHandle`
impl std::future::Future for PublisherJoinHandle {
    type Output = Result<(), Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        ctx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), Error>> {
        match self.as_mut().handle.poll_unpin(ctx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Err(err)) => {
                std::task::Poll::Ready(Err(Error::from(err).context("Joining Publisher")))
            }
            std::task::Poll::Ready(Ok(ok)) => std::task::Poll::Ready(ok),
        }
    }
}

/// Events for the Publisher event loop.
#[derive(Debug)]
enum PublisherEvent {
    /// Sent from the websocket receiver.
    WebSocket(ServerMessage),
    /// Sent from the GStreamer bus.
    GStreamer(gst::Message),
    /// Sent from anywhere if an error happens to report back.
    Error(anyhow::Error),
    /// Sent from stop() and other places.
    Close,
}

/// Wrapper around a `gst::Pipeline` to shut it down when dropped.
#[derive(Debug)]
struct PipelineWrapper(gst::Pipeline);

impl std::ops::Deref for PipelineWrapper {
    type Target = gst::Pipeline;

    fn deref(&self) -> &gst::Pipeline {
        &self.0
    }
}

impl Drop for PipelineWrapper {
    fn drop(&mut self) {
        debug!("Setting pipeline to Null state");
        let _ = self.0.set_state(gst::State::Null);
    }
}

impl Publisher {
    /// Run a new publisher in the background.
    ///
    /// This tries to connect to the configured server and tries to
    /// create the configured room, and otherwise fails directly.
    ///
    /// If the room can be created successfully it runs the publisher
    /// in the background until an error happens or it is stopped.
    pub async fn run(cfg: Config) -> Result<(Publisher, PublisherJoinHandle), Error> {
        let ws = Self::connect(&cfg).await.context("Connecting to server")?;

        // Channel for the publishers event loop
        let (event_sender, mut event_receiver) = mpsc::unbounded::<PublisherEvent>();

        // Channel for asynchronously sending out websocket message
        let (mut ws_sink, mut ws_stream) = ws.split();
        let (websocket_sender, mut websocket_receiver) = mpsc::unbounded::<PublisherMessage>();
        let websocket_send_task = tokio::spawn(async move {
            while let Some(msg) = websocket_receiver.next().await {
                trace!("Sending websocket message {:?}", msg);
                ws_sink
                    .send(WsMessage::Text(
                        serde_json::to_string(&msg).expect("Failed to serialize message"),
                    ))
                    .await?;
            }

            debug!("Closing websocket");
            ws_sink.send(WsMessage::Close(None)).await?;
            debug!("Closed websocket");
            ws_sink.close().await?;

            Ok::<(), Error>(())
        });

        // Read websocket messages and pass them as events to the publisher
        let mut event_sender_clone = event_sender.clone();
        tokio::spawn(async move {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(WsMessage::Text(msg)) => {
                        let msg = match serde_json::from_str::<ServerMessage>(&msg) {
                            Ok(msg) => msg,
                            Err(err) => {
                                warn!("Failed to deserialize server message: {:?}", err);
                                continue;
                            }
                        };

                        trace!("Received server message {:?}", msg);
                        if event_sender_clone
                            .send(PublisherEvent::WebSocket(msg))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(WsMessage::Close(reason)) => {
                        debug!("Websocket closed, reason: {:?}", reason);
                        let _ = event_sender_clone.send(PublisherEvent::Close).await;
                        break;
                    }
                    Ok(_) => {
                        warn!("Unsupported websocket message {:?}", msg);
                    }
                    Err(err) => {
                        let _ = event_sender_clone
                            .send(PublisherEvent::Error(
                                Error::from(err).context("Receiving websocket message"),
                            ))
                            .await;
                        break;
                    }
                }
            }

            debug!("Stopped websocket receiving");
        });

        // To remember if we already stopped before
        let stopped = Arc::new(atomic::AtomicBool::new(false));

        // Create pipeline and set up notifications with channels.
        let pipeline = PipelineWrapper(
            Publisher::new(&cfg, event_sender.clone(), websocket_sender.clone())
                .context("Creating publisher")?,
        );

        // Spawn our event loop
        let loop_join_handle = tokio::spawn(async move {
            // Start up pipeline asynchronously
            debug!("Setting publisher pipeline to Playing");

            pipeline
                .call_async_future(|pipeline| pipeline.set_state(gst::State::Playing))
                .await
                .context("Starting publisher pipeline")?;

            info!("Publisher running");

            // Handle all the events
            while let Some(event) = event_receiver.next().await {
                match event {
                    PublisherEvent::GStreamer(msg) => {
                        use gst::message::MessageView;

                        match msg.view() {
                            MessageView::Error(err) => {
                                let src = err.get_src().map(|src| src.get_path_string());
                                let src = src.as_deref().unwrap_or("UNKNOWN");
                                let dbg = err.get_debug();
                                let err = err.get_error();

                                if let Some(ref dbg) = dbg {
                                    error!("Got error from element {}: {} ({})", src, err, dbg);
                                } else {
                                    error!("Got error from element {}: {}", src, err);
                                }

                                bail!(
                                    "Error from element {}: {} ({})",
                                    src,
                                    err,
                                    dbg.unwrap_or_else(|| String::from("None")),
                                );
                            }
                            MessageView::Warning(warn) => {
                                let src = warn.get_src().map(|src| src.get_path_string());
                                let src = src.as_deref().unwrap_or("UNKNOWN");
                                let dbg = warn.get_debug();
                                let err = warn.get_error();

                                if let Some(ref dbg) = dbg {
                                    warn!("Got warning from element {}: {} ({})", src, err, dbg);
                                } else {
                                    warn!("Got warning from element {}: {}", src, err);
                                }
                            }
                            _ => (),
                        }
                    }
                    PublisherEvent::WebSocket(msg) => match msg {
                        ServerMessage::Sdp { type_, sdp } => match type_.as_str() {
                            "answer" => {
                                let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
                                    .map_err(|_| format_err!("Failed to parse SDP answer"))?;

                                debug!("Received SDP answer: {:#?}", sdp);

                                let answer = gst_webrtc::WebRTCSessionDescription::new(
                                    gst_webrtc::WebRTCSDPType::Answer,
                                    sdp,
                                );

                                let webrtcbin = pipeline
                                    .get_by_name("webrtcbin")
                                    .expect("Can't find webrtcbin");
                                webrtcbin
                                    .emit(
                                        "set-remote-description",
                                        &[&answer, &None::<gst::Promise>],
                                    )
                                    .expect("Failed to emit set-remote-description signal");
                            }
                            _ => {
                                error!("Unsupported SDP type {}", type_);
                                break;
                            }
                        },
                        ServerMessage::Ice {
                            sdp_mline_index,
                            candidate,
                        } => {
                            debug!(
                                "Adding remote ICE candidate: {} at SDP mline index {}",
                                candidate, sdp_mline_index
                            );

                            let webrtcbin = pipeline
                                .get_by_name("webrtcbin")
                                .expect("Can't find webrtcbin");
                            webrtcbin
                                .emit("add-ice-candidate", &[&sdp_mline_index, &candidate])
                                .expect("Failed to emit add-ice-candidate signal");
                        }
                        ServerMessage::Error { message } => {
                            error!("Got server error: {}", message);
                            bail!("Server error: {}", message);
                        }
                        ServerMessage::RoomCreated { .. } => {
                            error!("Unexpected RoomCreated server message");
                            continue;
                        }
                    },
                    PublisherEvent::Error(err) => {
                        error!("Received error {:?}, stopping", err);
                        return Err(err);
                    }
                    PublisherEvent::Close => {
                        info!("Shutting down");
                        websocket_sender.close_channel();
                        event_receiver.close();
                        websocket_send_task.await.context("Closing websocket")??;
                        break;
                    }
                }
            }

            // Stop pipeline asynchronously
            debug!("Setting publisher pipeline to Null");
            let _ = pipeline
                .call_async_future(|pipeline| pipeline.set_state(gst::State::Null))
                .await;

            Ok(())
        });

        Ok((
            Publisher {
                event_sender,
                stopped,
            },
            PublisherJoinHandle {
                handle: loop_join_handle,
            },
        ))
    }

    /// Connect to the WebSocket server and create a room.
    async fn connect(
        cfg: &Config,
    ) -> Result<async_tungstenite::WebSocketStream<impl AsyncRead + AsyncWrite>, Error> {
        debug!("Connecting to {}", cfg.server);

        // Connect to the configured server and create a room
        let (mut ws, _) = if let Some(ref certificate_file) = cfg.certificate_file {
            use openssl::ssl::{SslConnector, SslMethod};

            let mut builder = SslConnector::builder(SslMethod::tls())?;
            builder.set_ca_file(certificate_file)?;

            let connector = builder.build().configure()?;

            async_tungstenite::tokio::connect_async_with_tls_connector(&cfg.server, Some(connector))
                .await?
        } else {
            async_tungstenite::tokio::connect_async(&cfg.server).await?
        };

        debug!(
            "Connected to {}, creating room {} (description: {:?})",
            cfg.server, cfg.server_room, cfg.server_room_description
        );

        ws.send(WsMessage::Text(
            serde_json::to_string(&PublisherMessage::CreateRoom {
                name: cfg.server_room.clone(),
                description: cfg.server_room_description.clone(),
            })
            .expect("Failed to serialize create room message"),
        ))
        .await?;

        let msg = ws
            .next()
            .await
            .ok_or_else(|| format_err!("didn't receive anything"))??;

        let msg = match msg {
            WsMessage::Text(msg) => serde_json::from_str::<ServerMessage>(&msg)?,
            _ => bail!("Unexpected websocket message: {:?}", msg),
        };

        match msg {
            ServerMessage::RoomCreated { id } => {
                info!("Created room {}", id);
            }
            ServerMessage::Error { message } => {
                bail!("Error creating room: {}", message);
            }
            _ => bail!("Unexpected server message: {:?}", msg),
        }

        Ok(ws)
    }

    /// Create the pipeline and set up notifications.
    fn new(
        cfg: &Config,
        event_sender: mpsc::UnboundedSender<PublisherEvent>,
        websocket_sender: mpsc::UnboundedSender<PublisherMessage>,
    ) -> Result<gst::Pipeline, Error> {
        // Create the GStreamer pipeline
        let pipeline = gst::parse_launch(
            "audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=96 ! webrtcbin name=webrtcbin",
        )?
        .downcast::<gst::Pipeline>()
        .expect("Not a pipeline");

        let webrtcbin = pipeline
            .get_by_name("webrtcbin")
            .expect("Can't find webrtcbin");

        if let Some(ref stun_server) = &cfg.stun_server {
            webrtcbin
                .set_property("stun-server", stun_server)
                .expect("Failed to set stun-server property");
        }
        if let Some(ref turn_server) = &cfg.turn_server {
            webrtcbin
                .set_property("turn-server", turn_server)
                .expect("Failed to set turn-server property");
        }
        webrtcbin.set_property_from_str("bundle-policy", "max-bundle");

        // Spawn task for forwarding all GStreamer messages to our event loop
        let bus = pipeline.get_bus().expect("Pipeline without bus");
        let mut bus_stream = bus.stream();
        let mut event_sender_clone = event_sender.clone();
        tokio::spawn(async move {
            while let Some(msg) = bus_stream.next().await {
                if event_sender_clone
                    .send(PublisherEvent::GStreamer(msg))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Connect to on-negotiation-needed to handle sending an offer SDP
        let handle = tokio::runtime::Handle::current();
        let websocket_sender_clone = websocket_sender.clone();
        let event_sender_clone = event_sender.clone();
        webrtcbin
            .connect("on-negotiation-needed", false, move |values| {
                let webrtc = values[0]
                    .get::<gst::Element>()
                    .expect("Invalid argument")
                    .unwrap();

                let mut event_sender_clone = event_sender_clone.clone();
                let websocket_sender_clone = websocket_sender_clone.clone();
                handle.spawn(async move {
                    if let Err(err) = Self::start_negotiation(webrtc, websocket_sender_clone).await
                    {
                        let _ = event_sender_clone.send(PublisherEvent::Error(err)).await;
                    }
                });

                None
            })
            .unwrap();

        let websocket_sender_clone = websocket_sender.clone();
        webrtcbin
            .connect("on-ice-candidate", false, move |values| {
                let _webrtc = values[0].get::<gst::Element>().expect("Invalid argument");
                let sdp_mline_index = values[1].get_some::<u32>().expect("Invalid argument");
                let candidate = values[2]
                    .get::<String>()
                    .expect("Invalid argument")
                    .unwrap();

                let _ = websocket_sender_clone.unbounded_send(PublisherMessage::Ice {
                    candidate,
                    sdp_mline_index,
                });

                None
            })
            .unwrap();

        Ok(pipeline)
    }

    /// Stops the publisher.
    pub fn stop(&mut self) -> Result<(), Error> {
        if !self
            .stopped
            .compare_and_swap(false, true, atomic::Ordering::SeqCst)
        {
            info!("Stopping publisher");
            self.event_sender
                .unbounded_send(PublisherEvent::Close)
                .context("Stopping publisher")
        } else {
            Ok(())
        }
    }

    /// Starts negotiation, creates an offer and sends it to the peer.
    async fn start_negotiation(
        webrtcbin: gst::Element,
        mut websocket_sender: mpsc::UnboundedSender<PublisherMessage>,
    ) -> Result<(), Error> {
        // Create an offer
        debug!("Creating offer");
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

        let _ = websocket_sender
            .send(PublisherMessage::Sdp {
                type_: String::from("offer"),
                sdp: offer.get_sdp().as_text().expect("Invalid offer"),
            })
            .await;

        Ok(())
    }
}
