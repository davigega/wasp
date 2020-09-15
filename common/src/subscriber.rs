// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use serde::{Deserialize, Serialize};

/// Messages sent from the subscriber to the server.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriberMessage {
    JoinRoom {
        id: uuid::Uuid,
    },
    LeaveRoom,
    Ice {
        candidate: String,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: u32,
    },
    Sdp {
        #[serde(rename = "type")]
        type_: String,
        sdp: String,
    },
}

/// Messages sent from the the server to the subscriber.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerMessage {
    Error {
        message: String,
    },
    Ice {
        candidate: String,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: u32,
    },
    Sdp {
        #[serde(rename = "type")]
        type_: String,
        sdp: String,
    },
}
