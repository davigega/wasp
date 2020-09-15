// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use serde::{Deserialize, Serialize};

/// Messages sent from the publisher to the server.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublisherMessage {
    CreateRoom {
        name: String,
        description: Option<String>,
    },
    DeleteRoom,
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

/// Messages sent from the the server to the publisher.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerMessage {
    Error {
        message: String,
    },
    RoomCreated {
        id: uuid::Uuid,
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
