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
                latency: Some(cfg.server_latency),
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

    fn build_pipeline(cfg: &Config) -> Result<(gst::Pipeline, gst::Element), Error> {
        let pipeline = gst::Pipeline::new(None);
        let webrtcbin = gst::ElementFactory::make("webrtcbin", Some("webrtcbin"))
            .context("Creating webrtcbin")?;
        let rtpopuspay =
            gst::ElementFactory::make("rtpopuspay", None).context("Creating rtpopuspay")?;
        let opusenc = gst::ElementFactory::make("opusenc", None).context("Creating opusenc")?;
        let capsfilter =
            gst::ElementFactory::make("capsfilter", None).context("Creating capsfilter")?;
        let volume = gst::ElementFactory::make("volume", None).context("Creating volume")?;
        let audioconvert =
            gst::ElementFactory::make("audioconvert", None).context("Creating audioconvert")?;
        let audioresample =
            gst::ElementFactory::make("audioresample", None).context("Creating audioresample")?;
        let queue = gst::ElementFactory::make("queue", None).context("Creating queue")?;

        let source = match cfg
            .input_stream
            .splitn(2, ':')
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["pulseaudio", device] => {
                let src =
                    gst::ElementFactory::make("pulsesrc", None).context("Creating pulsesrc")?;
                src.set_property(
                    "device",
                    &(if *device == "default" {
                        None::<&str>
                    } else {
                        Some(*device)
                    }),
                )
                .expect("Failed to set device property on pulsesrc");

                src
            }
            ["jack", client_name] => {
                let bin = gst::Bin::new(None);

                let src = gst::ElementFactory::make("jackaudiosrc", None)
                    .context("Creating jackaudiosrc")?;
                src.set_property("client-name", client_name)
                    .expect("Failed to set client-name property on jackaudiosrc");
                src.set_property_from_str("connect", "none");

                let capsfilter =
                    gst::ElementFactory::make("capsfilter", None).context("Creating capsfilter")?;

                // Workaround for jackaudiosrc negotiating stereo if downstream
                // has converters before the other capsfilter
                let caps = gst::Caps::builder("audio/x-raw")
                    .field(
                        "channels",
                        &(match cfg.channel_configuration {
                            crate::config::ChannelConfiguration::Mono => 1i32,
                            crate::config::ChannelConfiguration::Stereo => 2i32,
                        }),
                    )
                    .build();
                capsfilter
                    .set_property("caps", &caps)
                    .expect("Failed to set caps property on capsfilter");

                bin.add_many(&[&src, &capsfilter])
                    .expect("Failed to add elements to jackaudiosrc bin");
                src.link(&capsfilter)
                    .expect("Failed to link jackaudiosrc to capsfilter");

                bin.add_pad(
                    &gst::GhostPad::with_target(
                        Some("src"),
                        &capsfilter
                            .get_static_pad("src")
                            .expect("No src pad in capsfilter"),
                    )
                    .expect("Failed to create ghost pad"),
                )
                .expect("Failed to add ghost pad to jackaudiosrc bin");

                bin.upcast()
            }
            ["audiotest", frequency] => {
                let src = gst::ElementFactory::make("audiotestsrc", None)
                    .context("Creating audiotestsrc")?;

                let freq = frequency
                    .parse::<f64>()
                    .context("Parsing audiotestsrc frequency")?;
                src.set_property("freq", &freq)
                    .expect("Failed to set freq property on audiotestsrc");
                src.set_property("is-live", &true)
                    .expect("Failed to set is-live property on audiotestsrc");

                src
            }
            _ => bail!("Unsupported input stream '{}'", cfg.input_stream),
        };

        pipeline
            .add_many(&[
                &source,
                &queue,
                &audioresample,
                &audioconvert,
                &volume,
                &capsfilter,
                &opusenc,
                &rtpopuspay,
                &webrtcbin,
            ])
            .expect("Can't add elements to pipeline");

        // This can't really fail: if anything it fails after starting the pipeline
        gst::Element::link_many(&[
            &source,
            &queue,
            &audioresample,
            &audioconvert,
            &volume,
            &capsfilter,
            &opusenc,
            &rtpopuspay,
            &webrtcbin,
        ])
        .expect("Failed to link elements");

        // Now configure all the elements
        volume
            .set_property("volume", &cfg.volume)
            .expect("Failed to set volume property on volume");
        opusenc
            .set_property("bitrate", &(cfg.bitrate as i32))
            .expect("Failed to set bitrate property on opusenc");
        rtpopuspay
            .set_property("pt", &96u32)
            .expect("Failed to set pt property on rtpopuspay");

        let caps = gst::Caps::builder("audio/x-raw")
            .field("rate", &(cfg.sample_rate as i32))
            .field(
                "channels",
                &(match cfg.channel_configuration {
                    crate::config::ChannelConfiguration::Mono => 1i32,
                    crate::config::ChannelConfiguration::Stereo => 2i32,
                }),
            )
            .build();
        capsfilter
            .set_property("caps", &caps)
            .expect("Failed to set caps property on capsfilter");

        queue
            .set_properties(&[
                ("max-size-bytes", &0u32),
                ("max-size-buffers", &0u32),
                ("max-size-time", &(100 * gst::MSECOND)),
            ])
            .expect("Can't set queue properties");

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

        Ok((pipeline, webrtcbin))
    }

    /// Create the pipeline and set up notifications.
    fn new(
        cfg: &Config,
        event_sender: mpsc::UnboundedSender<PublisherEvent>,
        websocket_sender: mpsc::UnboundedSender<PublisherMessage>,
    ) -> Result<gst::Pipeline, Error> {
        // Create the GStreamer pipeline
        let (pipeline, webrtcbin) = Self::build_pipeline(cfg)?;

        // Hook up the various callbacks for webrtcbin and the bus

        // Set transceiver as "sendonly". It was added above when linking the pipeline.
        let transceiver = webrtcbin
            .emit("get-transceiver", &[&0i32])
            .expect("Can't emit get-transceiver(0)")
            .unwrap()
            .get::<gst_webrtc::WebRTCRTPTransceiver>()
            .expect("Invalid type")
            .unwrap();
        transceiver
            .set_property(
                "direction",
                &gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
            )
            .unwrap();

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
